use std::path::PathBuf;

use crate::app::editor_layout::{self as lanes_layout, LaneGeometry};
use crate::app::tool_automation::{self, AutoParam};
use crate::app::ui::curve_canvas::{self, CurveAxis, CurveLook};
use crate::app::input_focus::UiSurface;
use crate::app::render::overlay as ov;
use crate::app::render::waveform_pyramid as wf_cache;
use crate::app::{helpers::*, types::*};
use crate::wave::build_minmax;
use egui::*;

pub(in crate::app) struct LoopSeamPreview {
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

fn waveform_render_lod(
    samples_per_device_column: f32,
    pyramid_base_samples: f32,
) -> WaveformRenderLod {
    if samples_per_device_column < 2.0 {
        WaveformRenderLod::Raw
    } else if samples_per_device_column < pyramid_base_samples.max(1.0) {
        WaveformRenderLod::VisibleMinMax
    } else {
        WaveformRenderLod::Pyramid
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpectroColorRangeHandle {
    Ceiling,
    Floor,
}

#[derive(Clone, Copy, Debug, Default)]
struct SpectroColorRangeEdit {
    changed: bool,
    committed: bool,
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
        self.fractional_sample_center_x_unclamped(display_sample as f64)
    }

    fn fractional_sample_center_x_unclamped(&self, display_sample: f64) -> f32 {
        let rel = ((display_sample + 0.5) - self.view_offset_exact) / self.spp.max(0.0001) as f64;
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

    /// Boundary x without the canvas clamp.
    ///
    /// `sample_boundary_x` clamps into the canvas, so a boundary scrolled off
    /// to the left reports exactly `wave_left` — and anything hit-testing
    /// against that finds a phantom handle pinned to the canvas edge. Handle
    /// code must use this and ask `contains_boundary` separately.
    fn sample_boundary_x_unclamped(&self, display_sample: usize) -> f32 {
        let rel = ((display_sample as f64) - self.view_offset_exact) / self.spp.max(0.0001) as f64;
        self.wave_left + rel as f32
    }

    fn contains_boundary(&self, display_sample: usize) -> bool {
        let x = self.sample_boundary_x_unclamped(display_sample);
        x >= self.wave_left && x <= self.wave_left + self.wave_w
    }
}

/// Grab radius for the primary selection's edge handles.
///
/// Deliberately the tightest of the waveform radii (loop markers 7, stretch 8,
/// edge fade 10, patchbay pins 18): these handles sit in the middle of the
/// canvas where a plain click is the seek gesture, so every extra pixel is a
/// pixel the user can no longer seek on. Staying strictly under the 8 px
/// playhead snap in `to_range_selection_display_sample` also means grabbing a
/// handle can never teleport the edge onto the playhead on the first frame.
const SELECTION_EDGE_GRAB_RADIUS: f32 = 7.0;

/// Grab radius for a loop edge on the waveform.
///
/// The same 7 px the selection's edges use, and for the same reason: the loop
/// line stands in the middle of the canvas where a plain click is the seek
/// gesture. It was three separate literals before — the hit test, the hover
/// cursor, and the stretch grip's "decline, a loop edge is here" test — which
/// is three chances for the grabbable area and the drawn one to drift apart.
const LOOP_EDGE_GRAB_RADIUS: f32 = 7.0;

/// How close a dragged loop edge has to come to a marker to land exactly on it.
///
/// Decided in pixels, not samples, so the magnet feels identical at every zoom.
/// Kept just above the grab radius, matching the playhead magnet the range drag
/// already uses: a snap that reached further than the grab would pull an edge
/// out from under the press on the frame it was picked up.
const LOOP_SNAP_RADIUS: f32 = 8.0;

/// The loop edge's grab tab: a downward triangle at the top of the loop line.
///
/// It sits immediately below the Time Stretch grip rather than at the very top
/// of the canvas, where it used to be drawn. The grip took that band over, so
/// the one glyph that says "grab here to move the loop" was standing exactly
/// where grabbing does something else entirely. It now starts where the loop
/// drag starts.
const LOOP_HANDLE_W: f32 = 11.0;
const LOOP_HANDLE_H: f32 = 9.0;

// The selection's Time Stretch grip: a tab at the very top of the canvas, and
// the only place the destructive stretch can be started from.
//
// Below this band the same edge line is a plain range resize, so what is drawn
// and what is grabbable must stay the same number — a grip that reaches
// further than it looks turns a range nudge into a resample.
const SELECTION_STRETCH_HANDLE_W: f32 = 10.0;
const SELECTION_STRETCH_HANDLE_H: f32 = 18.0;

/// Is `y` inside the stretch grip's band for a canvas whose top is `canvas_top`?
fn in_selection_stretch_band(canvas_top: f32, y: f32) -> bool {
    y >= canvas_top && y <= canvas_top + SELECTION_STRETCH_HANDLE_H
}

/// The grip's height, for pointer-driven UI tests that must aim inside it.
#[cfg(feature = "kittest")]
pub(crate) fn selection_stretch_handle_height() -> f32 {
    SELECTION_STRETCH_HANDLE_H
}

// The amplitude-0 line and the dB scale around it.
//
// These were one colour and one width, which made the line that says where the
// waveform is centred indistinguishable from the two that are only a scale.
// The centre line belongs to the same family as the zero lines the amplitude
// (`rgb(86, 98, 116)`) and time (`rgb(84, 94, 110)`) navigators already draw;
// the grid sits below it so the two roles read apart. Both stay well under the
// waveform's own brightness — this is a reference, not a feature.
const WAVE_CENTER_LINE_COL: Color32 = Color32::from_rgb(96, 104, 120);
const WAVE_CENTER_LINE_W: f32 = 1.4;
const WAVE_CENTER_LABEL_COL: Color32 = Color32::from_rgb(150, 158, 172);
const WAVE_DB_GRID_COL: Color32 = Color32::from_rgb(38, 38, 44);

/// Laid over a lane whose channel is muted (or lost to someone else's solo).
/// The canvas background at ~59% alpha, so the lane fades toward the backdrop
/// instead of changing hue. Written premultiplied because that is the `const`
/// constructor: rgb(16, 16, 18) x 150/255.
const WAVE_INAUDIBLE_SCRIM: Color32 = Color32::from_rgba_premultiplied(9, 9, 11, 150);

/// Nearest handle to `x` within `radius`, returning that handle's payload.
///
/// Ties resolve toward the later entry, so on a selection narrower than two
/// grab radii the end handle wins and the range extends outward — which is
/// the "lengthen it a bit" the feature is for. The loop-marker code does this
/// with `if / else if`, which silently always prefers the first candidate.
fn nearest_handle(candidates: &[(f32, usize)], x: f32, radius: f32) -> Option<usize> {
    let mut best: Option<(f32, usize)> = None;
    for &(handle_x, payload) in candidates {
        let distance = (x - handle_x).abs();
        if distance <= radius && best.is_none_or(|(d, _)| distance <= d) {
            best = Some((distance, payload));
        }
    }
    best.map(|(_, payload)| payload)
}

const SELECTION_STRETCH_MIN_RATE: f32 = 0.25;
const SELECTION_STRETCH_MAX_RATE: f32 = 4.0;

fn selection_stretch_target_len(
    source_len: usize,
    edge: SelectionStretchEdge,
    pointer_delta_px: f32,
    samples_per_px: f32,
) -> usize {
    let source_len = source_len.max(1);
    let sample_delta = pointer_delta_px as f64 * samples_per_px.max(0.0001) as f64;
    let raw_len = match edge {
        SelectionStretchEdge::Start => source_len as f64 - sample_delta,
        SelectionStretchEdge::End => source_len as f64 + sample_delta,
    };
    let min_len = ((source_len as f64) / SELECTION_STRETCH_MAX_RATE as f64)
        .ceil()
        .max(1.0) as usize;
    let max_len = source_len.saturating_mul(4).max(min_len);
    raw_len.round().clamp(min_len as f64, max_len as f64) as usize
}

fn selection_stretch_rate(source_len: usize, target_len: usize) -> f32 {
    ((source_len.max(1) as f64) / (target_len.max(1) as f64)).clamp(
        SELECTION_STRETCH_MIN_RATE as f64,
        SELECTION_STRETCH_MAX_RATE as f64,
    ) as f32
}

/// Spectral warp brush falloff (Gaussian sigma). In time: from a single
/// transient (10 ms) to a phrase (2 s). In frequency: from one harmonic's
/// width to most of the spectrum.
pub(in crate::app) const WARP_TIME_RADIUS_MS: std::ops::RangeInclusive<f32> = 10.0..=2_000.0;
pub(in crate::app) const WARP_FREQ_RADIUS_HZ: std::ops::RangeInclusive<f32> =
    crate::sample_rate::AUDIBLE_LOW_HZ..=8_000.0;

// Held arrow-key seeking: shared with the Multi Edits timeline.
use crate::app::ui_timing::{
    SEEK_REPEAT_ACCELERATE_AFTER, SEEK_REPEAT_DELAY, SEEK_REPEAT_FAST, SEEK_REPEAT_SLOW,
};

/// Mini meter analysis windows, in time so they read the same at any rate:
/// the oscilloscope shows 40 ms (a couple of cycles of a low voice), the
/// level meter integrates 50 ms (a fast PPM), the vectorscope 30 ms. The
/// frame bounds stop a very low or very high rate from making the window
/// uselessly short or expensively long.
const MINI_SCOPE_WINDOW_SECS: f64 = 0.04;
const MINI_LEVEL_WINDOW_SECS: f64 = 0.05;
const MINI_VECTOR_WINDOW_SECS: f64 = 0.03;
const MINI_METER_MIN_WINDOW_FRAMES: usize = 256;
const MINI_METER_MAX_WINDOW_FRAMES: usize = 8_192;
const MINI_VECTOR_MAX_WINDOW_FRAMES: usize = 4_096;

/// Loop seam preview: the shortest window asked for (2 ms, about one period
/// of a 500 Hz tone), the fewest frames shown each side of the seam however
/// short that is, and the longest stretch each side -- one second, whatever
/// the rate, so a long crossfade cannot turn the preview into a whole-file
/// copy.
const LOOP_SEAM_MIN_WINDOW_SECS: f32 = 0.002;
const LOOP_SEAM_MIN_SIDE_FRAMES: usize = 64;
const LOOP_SEAM_MAX_SIDE_SECS: f64 = 1.0;

/// How far a zero-crossing snap may move the cursor, each way. A second is
/// far beyond any real waveform's next crossing; the bound only stops a long
/// run of DC or silence from scanning the whole file.
const ZERO_CROSS_SNAP_MAX_SECS: f64 = 1.0;

/// Find the nearest zero crossing (in either direction, within
/// `ZERO_CROSS_SNAP_MAX_SECS`) using mixdown. `buffer_sr` is the rate of
/// `ch_samples`, so the window is the same length of time at any rate.
/// Used for Alt+drag snapping without holding a `&self` borrow.
pub(in crate::app) fn zc_snap_nearest(
    ch_samples: &[Vec<f32>],
    buffer_sr: u32,
    eps: f32,
    cur: usize,
) -> usize {
    let max_dist = crate::sample_rate::frames_for_secs(ZERO_CROSS_SNAP_MAX_SECS, buffer_sr);
    let ch_count = ch_samples.len();
    if ch_count == 0 {
        return cur;
    }
    let min_len = ch_samples.iter().map(|c| c.len()).min().unwrap_or(0);
    if min_len == 0 {
        return cur;
    }
    let cur = cur.min(min_len.saturating_sub(1));
    let eps = eps.max(0.0);
    let mix_at = |idx: usize| -> f32 {
        let sum: f32 = ch_samples.iter().filter_map(|c| c.get(idx).copied()).sum();
        sum / ch_count as f32
    };
    let is_cross =
        |a: f32, b: f32| -> bool { b.abs() <= eps || a.abs() <= eps || (a > 0.0) != (b > 0.0) };
    // `None` when that side has no crossing within reach. (It used to be
    // `cur`, which at distance 0 always beat a crossing found on the other
    // side, so a one-sided crossing was never snapped to.)
    let fwd = if cur + 1 < min_len {
        let mut prev = mix_at(cur);
        let mut found = None;
        let limit = min_len.min(cur + max_dist + 1);
        for i in (cur + 1)..limit {
            let s = mix_at(i);
            if is_cross(prev, s) {
                found = Some(i);
                break;
            }
            prev = s;
        }
        found
    } else {
        None
    };
    let bwd = if cur > 0 {
        let mut next = mix_at(cur);
        let mut found = None;
        let lo = cur.saturating_sub(max_dist);
        let mut i = cur - 1;
        loop {
            let s = mix_at(i);
            if is_cross(s, next) {
                found = Some(i);
                break;
            }
            next = s;
            if i <= lo {
                break;
            }
            i -= 1;
        }
        found
    } else {
        None
    };
    match (fwd, bwd) {
        (Some(f), Some(b)) if cur.abs_diff(f) <= cur.abs_diff(b) => f,
        (_, Some(b)) => b,
        (Some(f), None) => f,
        (None, None) => cur,
    }
}

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_editor_note_panel(
        ui: &mut egui::Ui,
        tab: &mut EditorTab,
        playhead_sample: usize,
        sample_rate: f32,
        request_seek: &mut Option<usize>,
    ) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label("Position");
            ui.selectable_value(
                &mut tab.editor_note_position_mode,
                EditorNotePositionMode::Time,
                "Time",
            );
            let beats = ui.add_enabled(
                tab.bpm_enabled && tab.bpm_value.is_finite() && tab.bpm_value > 0.0,
                egui::Button::selectable(
                    tab.editor_note_position_mode == EditorNotePositionMode::Beats,
                    "Beats",
                ),
            );
            if beats.clicked() {
                tab.editor_note_position_mode = EditorNotePositionMode::Beats;
            }
            beats.on_disabled_hover_text("Enable a valid BPM grid to show bars and beats");
        });

        let scope = if let Some((start, end)) = tab.selection {
            if let Some((low, high)) = tab.freq_selection {
                format!(
                    "Time + frequency: {}..{} / {:.0}..{:.0} Hz",
                    start.min(end),
                    start.max(end),
                    low.min(high),
                    low.max(high)
                )
            } else {
                format!("Time range: {}..{}", start.min(end), start.max(end))
            }
        } else {
            format!("Cursor: {playhead_sample}")
        };
        ui.label(RichText::new(scope).small().weak());
        ui.add(
            egui::TextEdit::multiline(&mut tab.editor_note_draft)
                .desired_rows(3)
                .hint_text("Write a comment for this position or selection..."),
        );
        let can_add = !tab.editor_note_draft.trim().is_empty();
        if ui
            .add_enabled(can_add, egui::Button::new("Add Editor Note"))
            .clicked()
        {
            let id = tab
                .editor_notes
                .iter()
                .map(|note| note.id)
                .max()
                .unwrap_or(0)
                .saturating_add(1);
            let normalized_selection = tab
                .selection
                .and_then(|(a, b)| (a != b).then_some((a.min(b), a.max(b))));
            let (start_sample, end_sample) = normalized_selection
                .map(|(a, b)| (a, Some(b)))
                .unwrap_or((playhead_sample, None));
            let freq_range_hz = normalized_selection
                .and(tab.freq_selection)
                .map(|(a, b)| (a.min(b), a.max(b)));
            tab.editor_notes.push(EditorNote {
                id,
                comment: tab.editor_note_draft.trim().to_string(),
                start_sample,
                end_sample,
                freq_range_hz,
                view: freq_range_hz.map(|_| format!("{:?}", tab.leaf_view_mode())),
            });
            tab.editor_notes
                .sort_by_key(|note| (note.start_sample, note.id));
            tab.editor_note_draft.clear();
            changed = true;
        }

        ui.separator();
        ui.label(RichText::new(format!("Notes ({})", tab.editor_notes.len())).strong());
        if tab.editor_notes.is_empty() {
            ui.label(RichText::new("No editor notes yet.").weak());
        }
        let position_mode = tab.editor_note_position_mode;
        let bpm_enabled = tab.bpm_enabled;
        let bpm = tab.bpm_value;
        let bpm_offset = tab.bpm_offset_sec;
        let beats_per_bar = tab.time_sig_numerator.max(1) as f64;
        let beat_unit = tab.time_sig_denominator.max(1) as f64 / 4.0;
        let format_position = |sample: usize| -> String {
            if position_mode == EditorNotePositionMode::Beats
                && bpm_enabled
                && bpm.is_finite()
                && bpm > 0.0
            {
                let seconds = sample as f64 / sample_rate.max(1.0) as f64;
                let absolute_beat =
                    ((seconds - bpm_offset as f64) * bpm as f64 / 60.0 * beat_unit).max(0.0);
                let bar = (absolute_beat / beats_per_bar).floor() as u64 + 1;
                let beat = absolute_beat % beats_per_bar + 1.0;
                format!("{bar}:{beat:.2}")
            } else {
                crate::app::helpers::format_time_s(sample as f32 / sample_rate.max(1.0))
            }
        };

        let mut delete_id = None;
        for note in tab.editor_notes.clone() {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let location = if let Some((start, end)) = note.normalized_range() {
                    format!("{} – {}", format_position(start), format_position(end))
                } else {
                    format_position(note.start_sample)
                };
                let location_response = ui
                    .add(
                        egui::Button::new(RichText::new(location).monospace().strong())
                            .frame(false),
                    )
                    .on_hover_text("Double-click to restore this cursor or selection");
                let click_time = std::time::Instant::now();
                let repeated_click = location_response.clicked()
                    && tab.editor_note_last_click.is_some_and(|(id, time)| {
                        id == note.id
                            && click_time.saturating_duration_since(time)
                                <= crate::app::ui_timing::DOUBLE_CLICK_WINDOW
                    });
                if location_response.clicked() {
                    tab.editor_note_last_click = Some((note.id, click_time));
                }
                if location_response.double_clicked() || repeated_click {
                    *request_seek = Some(note.start_sample);
                    tab.preview_offset_samples = Some(note.start_sample);
                    if let Some((start, end)) = note.normalized_range() {
                        tab.selection = Some((start, end));
                        tab.selection_anchor_sample = Some(start);
                        tab.freq_selection = note.freq_range_hz;
                    } else {
                        tab.selection = None;
                        tab.selection_anchor_sample = None;
                        tab.freq_selection = None;
                    }
                }
                if let Some((low, high)) = note.freq_range_hz {
                    ui.label(
                        RichText::new(format!(
                            "{} · {:.0}–{:.0} Hz",
                            note.view.as_deref().unwrap_or("Spectral"),
                            low,
                            high
                        ))
                        .small()
                        .weak(),
                    );
                }
                if tab.editor_note_editing_id == Some(note.id) {
                    ui.add(
                        egui::TextEdit::multiline(&mut tab.editor_note_edit_draft).desired_rows(2),
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !tab.editor_note_edit_draft.trim().is_empty(),
                                egui::Button::new("Save"),
                            )
                            .clicked()
                        {
                            if let Some(target) = tab
                                .editor_notes
                                .iter_mut()
                                .find(|target| target.id == note.id)
                            {
                                target.comment = tab.editor_note_edit_draft.trim().to_string();
                            }
                            tab.editor_note_editing_id = None;
                            tab.editor_note_edit_draft.clear();
                            changed = true;
                        }
                        if ui.button("Cancel").clicked() {
                            tab.editor_note_editing_id = None;
                            tab.editor_note_edit_draft.clear();
                        }
                    });
                } else {
                    ui.label(&note.comment);
                    ui.horizontal(|ui| {
                        if ui.small_button("Edit").clicked() {
                            tab.editor_note_editing_id = Some(note.id);
                            tab.editor_note_edit_draft = note.comment.clone();
                        }
                        if ui.small_button("Delete").clicked() {
                            delete_id = Some(note.id);
                        }
                    });
                }
            });
        }
        if let Some(id) = delete_id {
            tab.editor_notes.retain(|note| note.id != id);
            if tab.editor_note_editing_id == Some(id) {
                tab.editor_note_editing_id = None;
                tab.editor_note_edit_draft.clear();
            }
            changed = true;
        }
        changed
    }

    /// Timeline pixels to pan per pixel of wheel travel, for Shift+wheel.
    ///
    /// Sized so one ordinary notch — about `WHEEL_NOTCH_PX` of smooth delta —
    /// moves `HORIZONTAL_PAN_NOTCH_FRACTION` of the visible width at 1x. That
    /// keeps the gesture feeling the same whatever the zoom, and lets the
    /// speed multiplier mean something concrete: 4x is a little over half the
    /// viewport per notch.
    pub(crate) fn editor_horizontal_pan_gain(
        wave_w: f32,
        speed: EditorHorizontalScrollSpeed,
    ) -> f32 {
        const WHEEL_NOTCH_PX: f32 = 50.0;
        const HORIZONTAL_PAN_NOTCH_FRACTION: f32 = 0.15;
        wave_w.max(1.0) * HORIZONTAL_PAN_NOTCH_FRACTION / WHEEL_NOTCH_PX * speed.multiplier()
    }

    pub(crate) fn normalized_loop_range(range: Option<(usize, usize)>) -> Option<(usize, usize)> {
        range.map(|(a, b)| if a <= b { (a, b) } else { (b, a) })
    }

    /// Where a loop edge dragged to `x` should land.
    ///
    /// Markers win over zero crossings, which is the order that matters when
    /// both are in reach: a marker is a place the user put deliberately, a zero
    /// crossing is one the signal happens to offer. Zero crossings only apply
    /// while Alt is held; an ordinary drag remains sample-accurate.
    ///
    /// A snapped edge takes the marker's own sample index. Rounding back
    /// through the pixel it was drawn at would leave the loop a sample or two
    /// off the marker it is meant to sit exactly on. (The two still draw half a
    /// sample apart at extreme zoom, because a range edge lies *between*
    /// samples while a marker lies *on* one. The indices are equal, which is
    /// what loop export and playback read.)
    fn loop_edge_snap_sample(
        tab: &EditorTab,
        geom: &EditorDisplayGeometry,
        zero_cross_epsilon: f32,
        snap_to_zero_cross: bool,
        x: f32,
    ) -> usize {
        let mut nearest: Option<(f32, usize)> = None;
        for marker in &tab.markers {
            if !geom.contains_sample_center(marker.sample) {
                continue;
            }
            let dx = (geom.sample_center_x_unclamped(marker.sample) - x).abs();
            if dx <= LOOP_SNAP_RADIUS && nearest.is_none_or(|(best, _)| dx < best) {
                nearest = Some((dx, marker.sample));
            }
        }
        if let Some((_, sample)) = nearest {
            return sample;
        }
        let raw = geom.x_to_display_sample(x);
        if snap_to_zero_cross {
            return zc_snap_nearest(&tab.ch_samples, tab.buffer_sample_rate, zero_cross_epsilon, raw);
        }
        raw
    }

    pub(crate) fn resolve_editor_loop_visual_ranges(
        tab: &EditorTab,
    ) -> (Option<(usize, usize)>, Option<(usize, usize)>) {
        let editing = Self::normalized_loop_range(tab.loop_region);
        let applied = Self::normalized_loop_range(tab.loop_region_applied)
            .filter(|applied| Some(*applied) != editing);
        (applied, editing)
    }

    pub(in crate::app) fn build_loop_seam_preview(
        tab: &EditorTab,
        sample_rate: u32,
        window_secs: f32,
    ) -> Option<LoopSeamPreview> {
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
        let base_side_samples = crate::sample_rate::frames_for_secs(
            window_secs.max(LOOP_SEAM_MIN_WINDOW_SECS) as f64,
            sr,
        );
        let side_samples = base_side_samples
            .max(effective_xfade_samples.saturating_mul(2))
            .clamp(
                LOOP_SEAM_MIN_SIDE_FRAMES,
                crate::sample_rate::frames_for_secs(LOOP_SEAM_MAX_SIDE_SECS, sr)
                    .max(LOOP_SEAM_MIN_SIDE_FRAMES),
            );
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


    /// Small labelled confidence meter used by analysis panels.
    pub(in crate::app) fn inspector_confidence_bar(ui: &mut egui::Ui, confidence: f32) {
        let confidence = confidence.clamp(0.0, 1.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Confidence").weak());
            let bar = egui::ProgressBar::new(confidence)
                .desired_width((ui.available_width() - 44.0).max(48.0))
                .desired_height(10.0);
            ui.add(bar);
            ui.label(RichText::new(format!("{:.0}%", confidence * 100.0)).monospace());
        });
    }

    fn editor_view_label(view: ViewMode) -> &'static str {
        match view {
            ViewMode::Waveform => "Wave",
            ViewMode::Spectrogram => "Spec",
            ViewMode::Log => "Freq Log",
            ViewMode::Mel => "Mel",
            ViewMode::Tempogram => "Tempogram",
            ViewMode::Chromagram => "Chromagram",
            ViewMode::World => "World (F0/Env)",
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

    fn editor_fit_samples_per_px(display_samples_len: usize, wave_w: f32) -> f32 {
        (display_samples_len as f32 / wave_w.max(1.0)).max(crate::app::EDITOR_MIN_SAMPLES_PER_PX)
    }

    fn editor_is_whole_file_fit(tab: &EditorTab, display_samples_len: usize, wave_w: f32) -> bool {
        if display_samples_len == 0 || wave_w <= 0.0 || tab.samples_per_px <= 0.0 {
            return false;
        }
        if tab.view_offset > 1 || tab.view_offset_exact.abs() > 0.5 {
            return false;
        }
        let fit_spp = Self::editor_fit_samples_per_px(display_samples_len, wave_w);
        let visible_samples = tab.samples_per_px.max(0.0001) * wave_w.max(1.0);
        let covers_whole = visible_samples + 1.0 >= display_samples_len as f32;
        let close_to_fit = (tab.samples_per_px - fit_spp).abs() <= fit_spp.max(1.0) * 0.005;
        covers_whole || close_to_fit
    }

    /// Fit the view to the primary selection with a small margin (Z shortcut).
    pub(in crate::app) fn editor_zoom_to_selection(&mut self, tab_idx: usize) {
        let range = self.tabs.get(tab_idx).and_then(|tab| tab.selection);
        self.editor_zoom_to_range(tab_idx, range);
    }

    /// Fit the loop region to the view, the way `Z` fits the selection.
    ///
    /// Adjusting a loop means looking closely at both of its ends, and there
    /// was no way to get there except zooming by hand until they were both on
    /// screen.
    pub(in crate::app) fn editor_zoom_to_loop_region(&mut self, tab_idx: usize) {
        let range = self
            .tabs
            .get(tab_idx)
            .and_then(|tab| Self::normalized_loop_range(tab.loop_region));
        self.editor_zoom_to_range(tab_idx, range);
    }

    /// Fit `range` to the view with a small margin on each side.
    fn editor_zoom_to_range(&mut self, tab_idx: usize, range: Option<(usize, usize)>) {
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        let Some((start, end)) = range else {
            return;
        };
        if end <= start {
            return;
        }
        let wave_w = tab.last_wave_w;
        if wave_w <= 0.0 {
            return;
        }
        let display_samples_len = Self::editor_display_samples_len(tab);
        if display_samples_len == 0 {
            return;
        }
        let range_len = end - start;
        // 5% either side: enough that the edges are not flush against the
        // canvas border, which is what makes them awkward to grab.
        let pad = ((range_len as f64) * 0.05).ceil().max(1.0) as usize;
        let spp = Self::editor_fit_samples_per_px(range_len + pad * 2, wave_w);
        tab.samples_per_px = spp;
        let visible = ((wave_w * spp).ceil()).max(1.0) as usize;
        let max_left = display_samples_len.saturating_sub(visible);
        Self::editor_set_view_offset_exact(tab, start as f64 - pad as f64, max_left);
        Self::invalidate_editor_viewport_cache(tab);
    }

    /// Zoom in around one sample, keeping it in the middle of the view.
    ///
    /// Used by the double-click on a loop handle: the point you are working on
    /// stays where you are looking instead of sliding off as the view narrows.
    pub(in crate::app) fn editor_zoom_centered_on_sample(
        &mut self,
        tab_idx: usize,
        sample: usize,
        factor: f32,
    ) {
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        let wave_w = tab.last_wave_w;
        if wave_w <= 0.0 {
            return;
        }
        let display_samples_len = Self::editor_display_samples_len(tab);
        if display_samples_len == 0 {
            return;
        }
        let fit = Self::editor_fit_samples_per_px(display_samples_len, wave_w);
        let spp = (tab.samples_per_px * factor).clamp(
            crate::app::EDITOR_MIN_SAMPLES_PER_PX,
            fit.max(crate::app::EDITOR_MIN_SAMPLES_PER_PX),
        );
        tab.samples_per_px = spp;
        let visible = ((wave_w * spp).ceil()).max(1.0) as usize;
        let max_left = display_samples_len.saturating_sub(visible);
        let exact = Self::editor_exact_view_for_anchor(sample, 0.5, wave_w, spp);
        Self::editor_set_view_offset_exact(tab, exact, max_left);
        Self::invalidate_editor_viewport_cache(tab);
    }

    /// Keyboard zoom step (+/- keys) using the same 0.9/1.1 factor as the
    /// wheel, anchored on the playhead so the position under it stays put.
    pub(in crate::app) fn editor_zoom_step_at_playhead(&mut self, tab_idx: usize, zoom_in: bool) {
        let pos_audio = self
            .audio
            .shared
            .play_pos
            .load(std::sync::atomic::Ordering::Relaxed);
        let Some(tab_ro) = self.tabs.get(tab_idx) else {
            return;
        };
        let playhead_display = self.map_audio_to_display_sample(tab_ro, pos_audio);
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        let wave_w = tab.last_wave_w;
        if wave_w <= 0.0 {
            return;
        }
        let display_samples_len = Self::editor_display_samples_len(tab);
        if display_samples_len == 0 {
            return;
        }
        let factor = if zoom_in { 0.9 } else { 1.1 };
        // wave_left only offsets both sides of the ratio computation, so an
        // origin of 0 is fine outside the paint pass.
        Self::editor_apply_zoom_factor(
            tab,
            factor,
            EditorHorizontalZoomAnchorMode::Playhead,
            display_samples_len,
            0.0,
            wave_w,
            None,
            playhead_display,
        );
        Self::invalidate_editor_viewport_cache(tab);
    }

    /// Shift the view one visible page back/forward ([ / ] keys).
    pub(in crate::app) fn editor_view_page(&mut self, tab_idx: usize, forward: bool) {
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        let wave_w = tab.last_wave_w;
        if wave_w <= 0.0 {
            return;
        }
        let display_samples_len = Self::editor_display_samples_len(tab);
        if display_samples_len == 0 {
            return;
        }
        let vis = ((wave_w * tab.samples_per_px.max(0.0001)).ceil()).max(1.0) as usize;
        let max_left = display_samples_len.saturating_sub(vis);
        let step = if forward { vis as f64 } else { -(vis as f64) };
        let next_exact = tab.view_offset_exact + step;
        Self::editor_set_view_offset_exact(tab, next_exact, max_left);
        Self::invalidate_editor_viewport_cache(tab);
    }

    fn editor_selection_anchor_or(tab: &EditorTab, fallback: usize) -> usize {
        tab.selection_anchor_sample
            .or_else(|| tab.selection.map(|(a, b)| a.min(b)))
            .unwrap_or(fallback)
    }

    /// Frequency-axis top value for the spectral views: Nyquist of the
    /// analysis, optionally capped by the display config (`max_freq_hz`).
    /// Must stay in sync with the axis ticks and the viewport renderer.
    fn editor_spec_axis_max_freq(cfg: &SpectrogramConfig, sr: u32) -> f32 {
        let mut max_freq = (sr.max(1) as f32) * 0.5;
        if cfg.max_freq_hz > 0.0 {
            max_freq = cfg.max_freq_hz.min(max_freq).max(1.0);
        }
        max_freq
    }

    /// Vertical axis fraction (0 = bottom, 1 = top of the full axis) → Hz
    /// for the spectral views. Inverse of [`Self::editor_spec_freq_to_frac`].
    fn editor_spec_frac_to_freq(
        view_mode: ViewMode,
        frac: f32,
        max_freq: f32,
        mel_scale: SpectrogramScale,
    ) -> f32 {
        let frac = frac.clamp(0.0, 1.0);
        let log_min = 20.0_f32.min(max_freq).max(1.0);
        match view_mode {
            ViewMode::Spectrogram => frac * max_freq,
            ViewMode::Log => {
                if max_freq <= log_min {
                    frac * max_freq
                } else {
                    log_min * (max_freq / log_min).powf(frac)
                }
            }
            ViewMode::Mel => {
                let mel_max = 2595.0 * (1.0 + max_freq / 700.0).log10();
                let mel_min = 1.0_f32;
                let mel = match mel_scale {
                    SpectrogramScale::Linear => mel_max * frac,
                    SpectrogramScale::Log => {
                        if mel_max <= mel_min {
                            mel_max * frac
                        } else {
                            mel_min * (mel_max / mel_min).powf(frac)
                        }
                    }
                };
                700.0 * (10.0_f32.powf(mel / 2595.0) - 1.0)
            }
            _ => frac * max_freq,
        }
    }

    /// Hz → vertical axis fraction for the spectral views. Mirrors the
    /// tick placement in the frequency gutter exactly.
    fn editor_spec_freq_to_frac(
        view_mode: ViewMode,
        freq: f32,
        max_freq: f32,
        mel_scale: SpectrogramScale,
    ) -> f32 {
        let freq = freq.clamp(0.0, max_freq);
        let log_min = 20.0_f32.min(max_freq).max(1.0);
        match view_mode {
            ViewMode::Spectrogram => (freq / max_freq).clamp(0.0, 1.0),
            ViewMode::Log => {
                if freq <= 0.0 || max_freq <= log_min {
                    0.0
                } else {
                    let f = freq.clamp(log_min, max_freq);
                    (f / log_min).ln() / (max_freq / log_min).ln()
                }
            }
            ViewMode::Mel => {
                let mel_max = 2595.0 * (1.0 + max_freq / 700.0).log10();
                let mel_min = 1.0_f32;
                let mel = 2595.0 * (1.0 + (freq / 700.0)).log10();
                match mel_scale {
                    SpectrogramScale::Linear => (mel / mel_max).clamp(0.0, 1.0),
                    SpectrogramScale::Log => {
                        if mel_max <= mel_min {
                            (mel / mel_max.max(1.0)).clamp(0.0, 1.0)
                        } else {
                            (mel / mel_min).ln().clamp(0.0, (mel_max / mel_min).ln())
                                / (mel_max / mel_min).ln()
                        }
                    }
                }
            }
            _ => 0.0,
        }
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

    pub(in crate::app) fn editor_set_selection_from_anchor(
        tab: &mut EditorTab,
        anchor: usize,
        target: usize,
    ) {
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

    /// Shared tail of every multiplicative zoom (keyboard, arrow keys,
    /// wheel/pinch): clamp the new samples-per-px between max zoom-in and
    /// fit-to-width, then re-anchor the view so the sample under the anchor
    /// keeps its screen position. Viewport-cache invalidation stays with the
    /// caller (paint-pass sites intentionally skip it).
    #[allow(clippy::too_many_arguments)]
    fn editor_apply_zoom_factor(
        tab: &mut EditorTab,
        factor: f32,
        anchor_mode: EditorHorizontalZoomAnchorMode,
        display_samples_len: usize,
        wave_left: f32,
        wave_w: f32,
        pointer_x: Option<f32>,
        playhead_display: usize,
    ) {
        let old_spp = tab.samples_per_px.max(0.0001);
        let (anchor, t) = Self::editor_zoom_anchor(
            anchor_mode,
            tab,
            display_samples_len,
            wave_left,
            wave_w,
            pointer_x,
            playhead_display,
        );
        let min_spp = crate::app::EDITOR_MIN_SAMPLES_PER_PX;
        let max_spp_fit = Self::editor_fit_samples_per_px(display_samples_len, wave_w);
        let new_spp = (old_spp * factor).clamp(min_spp, max_spp_fit);
        tab.samples_per_px = new_spp;
        let geom = EditorDisplayGeometry::new(
            wave_left,
            wave_w,
            new_spp,
            tab.view_offset,
            tab.view_offset_exact,
            display_samples_len,
        );
        let next_exact = Self::editor_exact_view_for_anchor(anchor, t, wave_w, new_spp);
        Self::editor_set_view_offset_exact(tab, next_exact, geom.max_left());
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

    /// Boundary x for a display sample — where the selection rectangle and its
    /// grab handles are drawn. `editor_display_sample_x_for_tab` returns the
    /// sample *centre*, which is half a sample-per-pixel away and at deep zoom
    /// misses the 7 px grab radius entirely.
    #[cfg(feature = "kittest")]
    pub(crate) fn editor_display_sample_boundary_x_for_tab(
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
        .sample_boundary_x(display_sample)
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

    /// Is this channel actually being monitored right now?
    ///
    /// Mirrors the engine's own rule (`audible_mask` in `src/audio.rs`): a solo
    /// anywhere silences everything that is not soloed, and only when nothing
    /// is soloed does mute decide.
    pub(crate) fn editor_channel_is_audible(tab: &EditorTab, channel: usize) -> bool {
        if tab.ch_solo.iter().any(|&s| s) {
            tab.ch_solo.get(channel).copied().unwrap_or(false)
        } else {
            !tab.ch_muted.get(channel).copied().unwrap_or(false)
        }
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
                let desired = egui::vec2(desired_width.max(120.0), lanes_layout::TIME_NAV_H);
                let (navigator_resp, painter) =
                    ui.allocate_painter(desired, Sense::click_and_drag());
                rect = navigator_resp.rect;
                resp = Some(navigator_resp);

                painter.rect_filled(rect, 6.0, Color32::from_rgb(14, 16, 20));
                painter.rect_stroke(
                    rect,
                    6.0,
                    Stroke::new(1.0_f32, Color32::from_rgb(46, 54, 66)),
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
                            Stroke::new(1.0_f32, Color32::from_rgb(120, 138, 162)),
                        );
                    }
                } else {
                    painter.line_segment(
                        [
                            egui::pos2(wave_rect.left(), center_y),
                            egui::pos2(wave_rect.right(), center_y),
                        ],
                        Stroke::new(1.0_f32, Color32::from_rgb(84, 94, 110)),
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
                    Stroke::new(1.5_f32, Color32::from_rgb(92, 188, 255)),
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

    /// The colour an automation lane is drawn in: an EQ band's own colour
    /// (as on the EQ graph), the threshold / ceiling colours of the dynamics
    /// panels for those tools.
    fn automation_color(param: AutoParam) -> Color32 {
        use crate::eq::EqParam;
        match param {
            AutoParam::Eq(EqParam::Output) => Color32::from_rgb(230, 230, 236),
            AutoParam::Eq(p) => crate::app::ui::inspector::plots::BAND_COLORS
                [p.band().unwrap_or(0) % crate::app::ui::inspector::plots::BAND_COLORS.len()],
            AutoParam::Comp(_) => Color32::from_rgb(255, 170, 70),
            AutoParam::Limiter(_) => Color32::from_rgb(255, 120, 120),
        }
    }

    /// "Ch N" (and the speaker, when the layout names one) in the bottom-left
    /// corner of each lane on screen. Bottom, not top: the time ruler's
    /// labels run along the canvas's top edge.
    fn draw_editor_lane_labels(
        painter: &egui::Painter,
        lanes: LaneGeometry,
        visible: std::ops::Range<usize>,
        lane_channels: &[usize],
        layout: Option<&[Option<crate::audio_channels::SpeakerPos>]>,
        wave_left: f32,
    ) {
        let font = egui::FontId::monospace(10.0);
        for lane in visible {
            let Some(&channel) = lane_channels.get(lane) else {
                continue;
            };
            let speaker = layout.and_then(|layout| {
                layout
                    .get(channel)
                    .copied()
                    .flatten()
                    .map(|pos| pos.label(layout))
            });
            let text = match speaker {
                Some(name) => format!("Ch {} {name}", channel + 1),
                None => format!("Ch {}", channel + 1),
            };
            let galley = painter.layout_no_wrap(text, font.clone(), Color32::from_rgb(178, 188, 204));
            let anchor = egui::pos2(wave_left + 4.0, lanes.lane_top(lane + 1) - 3.0);
            let text_rect = egui::Align2::LEFT_BOTTOM.anchor_size(anchor, galley.size());
            painter.rect_filled(
                text_rect.expand2(egui::vec2(3.0, 1.0)),
                3.0,
                Color32::from_rgba_unmultiplied(10, 11, 14, 180),
            );
            painter.galley(text_rect.min, galley, Color32::WHITE);
        }
    }

    /// The lane scrollbar, shown while the lanes overflow the canvas. Drag
    /// the thumb to scroll; click the track beside it to page. Returns where
    /// it scrolled to, if it did. Interacted after the canvas, so it is on
    /// top of the canvas's own click-and-drag (as the amplitude rail is).
    fn draw_editor_lane_scrollbar(
        ui: &mut egui::Ui,
        bar_rect: egui::Rect,
        lanes: LaneGeometry,
    ) -> Option<f32> {
        let resp = ui
            .interact(
                bar_rect,
                ui.id().with("editor_lane_scrollbar"),
                Sense::click_and_drag(),
            )
            .on_hover_text(
                "Scroll the channel lanes. The wheel here or over the left scale, \
                 or Alt+wheel over the waveform, scrolls them too.",
            );
        let mut next = None;
        if resp.dragged() {
            next = Some(lanes.scroll_after_thumb_drag(resp.drag_delta().y, bar_rect.height()));
        } else if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                next = Some(lanes.scroll_after_track_click(pos.y, bar_rect.top(), bar_rect.height()));
            }
        }
        let shown = next.map_or(lanes, |scroll_px| LaneGeometry { scroll_px, ..lanes });
        let (thumb_top, thumb_h) = shown.thumb_span(bar_rect.top(), bar_rect.height());
        let thumb = egui::Rect::from_min_size(
            egui::pos2(bar_rect.left(), thumb_top),
            egui::vec2(bar_rect.width(), thumb_h),
        );
        let painter = ui.painter_at(bar_rect);
        painter.rect_filled(bar_rect, 4.0, Color32::from_rgb(14, 16, 20));
        let thumb_col = if resp.dragged() {
            Color32::from_rgb(92, 188, 255)
        } else if resp.hovered() {
            Color32::from_rgb(110, 124, 146)
        } else {
            Color32::from_rgb(72, 82, 98)
        };
        painter.rect_filled(thumb.shrink2(egui::vec2(1.5, 1.0)), 3.0, thumb_col);
        next.filter(|scroll_px| *scroll_px != lanes.scroll_px)
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
            ui.id().with("editor_amplitude_nav"),
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
            Stroke::new(1.0_f32, Color32::from_rgb(46, 54, 66)),
            egui::StrokeKind::Outside,
        );
        let zero_y = Self::amplitude_nav_y_from_amp(rail_rect, 0.0);
        painter.line_segment(
            [
                egui::pos2(rail_rect.left(), zero_y),
                egui::pos2(rail_rect.right(), zero_y),
            ],
            Stroke::new(1.0_f32, Color32::from_rgb(86, 98, 116)),
        );
        painter.rect_filled(
            viewport_rect,
            4.0,
            Color32::from_rgba_unmultiplied(72, 160, 255, 34),
        );
        painter.rect_stroke(
            viewport_rect,
            4.0,
            Stroke::new(1.5_f32, Color32::from_rgb(92, 188, 255)),
            egui::StrokeKind::Outside,
        );
        for y in [viewport_rect.top(), viewport_rect.bottom()] {
            painter.line_segment(
                [
                    egui::pos2(viewport_rect.left() + 2.0, y),
                    egui::pos2(viewport_rect.right() - 2.0, y),
                ],
                Stroke::new(2.0_f32, Color32::from_rgb(110, 210, 255)),
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

    fn spectro_color_range_y(rect: egui::Rect, db: f32) -> f32 {
        // The colour-range bar spans the whole adjustable range, 0 dB at the
        // top down to the lowest floor at the bottom.
        let span = -crate::levels::SPECTRO_DB_FLOOR_MIN;
        rect.top() + ((0.0 - db.clamp(crate::levels::SPECTRO_DB_FLOOR_MIN, 0.0)) / span) * rect.height()
    }

    fn spectro_color_range_db(rect: egui::Rect, y: f32) -> f32 {
        let t = ((y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
        crate::levels::SPECTRO_DB_FLOOR_MIN * t
    }

    fn set_spectro_color_range_handle(
        cfg: &mut SpectrogramConfig,
        handle: SpectroColorRangeHandle,
        db: f32,
    ) {
        match handle {
            SpectroColorRangeHandle::Ceiling => {
                cfg.db_ceiling = db
                    .clamp(
                        (cfg.db_floor + crate::levels::SPECTRO_MIN_SPAN_DB).max(crate::levels::SPECTRO_DB_CEILING_MIN),
                        crate::levels::SPECTRO_DB_CEILING_MAX,
                    )
                    .clamp(crate::levels::SPECTRO_DB_CEILING_MIN, crate::levels::SPECTRO_DB_CEILING_MAX);
            }
            SpectroColorRangeHandle::Floor => {
                cfg.db_floor = db
                    .clamp(
                        crate::levels::SPECTRO_DB_FLOOR_MIN,
                        (cfg.db_ceiling - crate::levels::SPECTRO_MIN_SPAN_DB).min(crate::levels::SPECTRO_DB_FLOOR_MAX),
                    )
                    .clamp(crate::levels::SPECTRO_DB_FLOOR_MIN, crate::levels::SPECTRO_DB_FLOOR_MAX);
            }
        }
    }

    fn draw_editor_spectro_color_range(
        ui: &mut egui::Ui,
        rect: egui::Rect,
        cfg: &mut SpectrogramConfig,
    ) -> SpectroColorRangeEdit {
        let before = (cfg.db_floor, cfg.db_ceiling);
        let response = ui.interact(
            rect,
            ui.id().with("editor_spectro_color_range"),
            Sense::click_and_drag(),
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Slider,
                ui.is_enabled(),
                "Spectrogram color range",
            )
        });
        let response = response.on_hover_text(format!(
            "Spectrogram color range: {:.0} to {:.0} {}\nDrag either handle; double-click to reset.",
            cfg.db_floor,
            cfg.db_ceiling,
            match cfg.db_ref {
                SpectrogramDbRef::Absolute => "dBFS",
                SpectrogramDbRef::MaxNormalized => "dB relative to max",
            }
        ));
        let drag_id = response.id.with("drag_handle");
        let selected_id = response.id.with("selected_handle");
        let last_click_id = response.id.with("last_click");
        let mut active_handle = ui
            .ctx()
            .memory(|mem| mem.data.get_temp::<SpectroColorRangeHandle>(drag_id));
        let mut committed = false;

        let (press_pos, input_time) = ui.input(|i| {
            let press_pos = i.events.iter().find_map(|event| match event {
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    ..
                } if rect.contains(*pos) => Some(*pos),
                _ => None,
            });
            (press_pos, i.time)
        });
        let mut repeated_click = false;
        if let Some(pos) = press_pos {
            repeated_click = ui.ctx().memory(|mem| {
                mem.data
                    .get_temp::<(f64, egui::Pos2)>(last_click_id)
                    .is_some_and(|(previous_time, previous_pos)| {
                        input_time - previous_time <= 0.8 && previous_pos.distance(pos) <= 6.0
                    })
            });
            ui.ctx().memory_mut(|mem| {
                mem.data.insert_temp(last_click_id, (input_time, pos));
            });
            response.request_focus();
            let ceiling_y = Self::spectro_color_range_y(rect, cfg.db_ceiling);
            let floor_y = Self::spectro_color_range_y(rect, cfg.db_floor);
            let handle = if (pos.y - ceiling_y).abs() <= (pos.y - floor_y).abs() {
                SpectroColorRangeHandle::Ceiling
            } else {
                SpectroColorRangeHandle::Floor
            };
            active_handle = Some(handle);
            ui.ctx().memory_mut(|mem| {
                mem.data.insert_temp(drag_id, handle);
                mem.data.insert_temp(selected_id, handle);
            });
            Self::set_spectro_color_range_handle(
                cfg,
                handle,
                Self::spectro_color_range_db(rect, pos.y),
            );
        }

        if response.dragged() {
            if let (Some(handle), Some(pos)) = (active_handle, response.interact_pointer_pos()) {
                Self::set_spectro_color_range_handle(
                    cfg,
                    handle,
                    Self::spectro_color_range_db(rect, pos.y),
                );
            }
        }

        if response.double_clicked() || repeated_click {
            cfg.db_floor = crate::levels::SPECTRO_DB_FLOOR_DEFAULT;
            cfg.db_ceiling = crate::levels::SPECTRO_DB_CEILING_MAX;
            committed = true;
            ui.ctx().memory_mut(|mem| {
                mem.data.remove::<SpectroColorRangeHandle>(drag_id);
            });
        }

        if response.has_focus() {
            let up = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
            let down = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
            if up || down {
                let handle = ui
                    .ctx()
                    .memory(|mem| mem.data.get_temp::<SpectroColorRangeHandle>(selected_id))
                    .unwrap_or(SpectroColorRangeHandle::Ceiling);
                let current = match handle {
                    SpectroColorRangeHandle::Ceiling => cfg.db_ceiling,
                    SpectroColorRangeHandle::Floor => cfg.db_floor,
                };
                Self::set_spectro_color_range_handle(
                    cfg,
                    handle,
                    current + if up { 1.0 } else { -1.0 },
                );
                committed = true;
            }
        }

        if response.drag_stopped() {
            committed = true;
            ui.ctx().memory_mut(|mem| {
                mem.data.remove::<SpectroColorRangeHandle>(drag_id);
            });
        } else if response.clicked() {
            committed = true;
        }

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, Color32::from_rgb(13, 15, 19));
        painter.rect_stroke(
            rect,
            4.0,
            Stroke::new(1.0_f32, Color32::from_rgb(46, 54, 66)),
            egui::StrokeKind::Inside,
        );
        let gradient_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 3.0, rect.top() + 3.0),
            egui::pos2(rect.left() + 12.0, rect.bottom() - 3.0),
        );
        const GRADIENT_STEPS: usize = 64;
        for step in 0..GRADIENT_STEPS {
            let y0 =
                gradient_rect.top() + gradient_rect.height() * step as f32 / GRADIENT_STEPS as f32;
            let y1 = gradient_rect.top()
                + gradient_rect.height() * (step + 1) as f32 / GRADIENT_STEPS as f32;
            let brightness = 1.0 - (step as f32 + 0.5) / GRADIENT_STEPS as f32;
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(gradient_rect.left(), y0),
                    egui::pos2(gradient_rect.right(), y1 + 1.0),
                ),
                0.0,
                crate::app::helpers::colormap_at(brightness),
            );
        }
        let ceiling_y = Self::spectro_color_range_y(rect, cfg.db_ceiling);
        let floor_y = Self::spectro_color_range_y(rect, cfg.db_floor);
        if ceiling_y > rect.top() {
            painter.rect_filled(
                egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), ceiling_y)),
                0.0,
                Color32::from_black_alpha(150),
            );
        }
        if floor_y < rect.bottom() {
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(rect.left(), floor_y), rect.max),
                0.0,
                Color32::from_black_alpha(150),
            );
        }
        let tick_font = FontId::monospace(8.0);
        for db in (0..=160).step_by(20) {
            let value = if db == 0 { 0.0 } else { -(db as f32) };
            let y = Self::spectro_color_range_y(rect, value);
            painter.line_segment(
                [
                    egui::pos2(rect.left() + 12.0, y),
                    egui::pos2(rect.left() + 15.0, y),
                ],
                Stroke::new(1.0_f32, Color32::from_rgb(112, 122, 138)),
            );
            painter.text(
                egui::pos2(rect.left() + 17.0, y),
                egui::Align2::LEFT_CENTER,
                format!("{value:.0}"),
                tick_font.clone(),
                Color32::from_rgb(154, 164, 180),
            );
        }
        for (y, color) in [
            (ceiling_y, Color32::from_rgb(245, 245, 250)),
            (floor_y, Color32::from_rgb(92, 188, 255)),
        ] {
            painter.line_segment(
                [
                    egui::pos2(rect.left() + 1.0, y),
                    egui::pos2(rect.right() - 1.0, y),
                ],
                Stroke::new(2.0_f32, color),
            );
            painter.circle_filled(egui::pos2(rect.left() + 8.0, y), 3.0, color);
        }
        if response.hovered() || active_handle.is_some() {
            ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeVertical);
        }

        SpectroColorRangeEdit {
            changed: before != (cfg.db_floor, cfg.db_ceiling),
            committed,
        }
    }

    /// Seam-continuity check: the tail running into the loop end and the
    /// head starting at the loop start drawn as ONE continuous trace, joined
    /// at the jump. A smooth line across the center means the loop connects;
    /// a step or kink means an audible click. The crossfaded result (if any)
    /// is overlaid in green, and the amplitude jump at the joint is reported.
    pub(in crate::app) fn draw_loop_seam_join(ui: &mut egui::Ui, preview: &LoopSeamPreview, window_ms: f32) {
        let sr = preview.sample_rate.max(1) as f32;
        let want = ((window_ms / 1000.0) * sr).round().max(16.0) as usize;
        let n = want
            .min(preview.raw_left.len())
            .min(preview.raw_right.len())
            .max(2);
        let tail = &preview.raw_left[preview.raw_left.len() - n..];
        let head = &preview.raw_right[..n];

        // What actually plays across the jump (with crossfade baked) if staged.
        let blended: Option<Vec<f32>> = match (&preview.blended_left, &preview.blended_right) {
            (Some(left), Some(right)) if left.len() >= n && right.len() >= n => {
                let mut joined = Vec::with_capacity(n * 2);
                joined.extend_from_slice(&left[left.len() - n..]);
                joined.extend_from_slice(&right[..n]);
                Some(joined)
            }
            _ => None,
        };
        let mut joined_raw = Vec::with_capacity(n * 2);
        joined_raw.extend_from_slice(tail);
        joined_raw.extend_from_slice(head);

        // Auto gain: scale the visible window to fill the lane so small
        // discontinuities stay visible regardless of material loudness.
        let peak = joined_raw
            .iter()
            .chain(blended.iter().flatten())
            .fold(0.0f32, |acc, v| acc.max(v.abs()))
            .max(1.0e-6);
        let gain = 0.92 / peak;

        let desired = egui::vec2(ui.available_width().max(160.0), 120.0);
        let (resp, painter) = ui.allocate_painter(desired, Sense::hover());
        let rect = resp.rect;
        painter.rect_filled(rect, 6.0, Color32::from_rgb(15, 18, 24));
        painter.rect_stroke(
            rect,
            6.0,
            Stroke::new(1.0_f32, Color32::from_rgb(52, 62, 78)),
            egui::StrokeKind::Outside,
        );
        let wave_rect = rect.shrink2(egui::vec2(8.0, 8.0));
        let wave_rect = egui::Rect::from_min_max(
            wave_rect.min,
            egui::pos2(wave_rect.max.x, wave_rect.max.y - 26.0),
        );
        let mid_y = wave_rect.center().y;
        let amp_to_y =
            |v: f32| -> f32 { mid_y - (v * gain).clamp(-1.0, 1.0) * wave_rect.height() * 0.5 };
        // Zero line + jump marker.
        painter.line_segment(
            [
                egui::pos2(wave_rect.left(), mid_y),
                egui::pos2(wave_rect.right(), mid_y),
            ],
            Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(120, 140, 170, 36)),
        );
        let seam_x = wave_rect.center().x;
        painter.line_segment(
            [
                egui::pos2(seam_x, wave_rect.top()),
                egui::pos2(seam_x, wave_rect.bottom()),
            ],
            Stroke::new(1.5_f32, Color32::from_rgb(255, 196, 72)),
        );
        let draw_trace = |painter: &egui::Painter, samples: &[f32], stroke: Stroke| {
            let total = samples.len();
            if total < 2 {
                return;
            }
            let width_px = wave_rect.width().max(8.0);
            if total as f32 <= width_px * 1.5 {
                // Per-sample polyline: continuity (steps/kinks) stays visible.
                let denom = (total - 1) as f32;
                let points: Vec<egui::Pos2> = samples
                    .iter()
                    .enumerate()
                    .map(|(i, &v)| {
                        egui::pos2(
                            egui::lerp(wave_rect.x_range(), i as f32 / denom),
                            amp_to_y(v),
                        )
                    })
                    .collect();
                painter.add(egui::Shape::line(points, stroke));
            } else {
                let bins = width_px as usize;
                let mut tmp = Vec::new();
                build_minmax(&mut tmp, samples, bins);
                let denom = (tmp.len().saturating_sub(1)).max(1) as f32;
                let mut mids = Vec::with_capacity(tmp.len());
                for (i, (mn, mx)) in tmp.iter().enumerate() {
                    let x = egui::lerp(wave_rect.x_range(), i as f32 / denom);
                    painter.line_segment(
                        [egui::pos2(x, amp_to_y(*mn)), egui::pos2(x, amp_to_y(*mx))],
                        Stroke::new(1.0_f32, stroke.color.gamma_multiply(0.4)),
                    );
                    mids.push(egui::pos2(x, amp_to_y((mn + mx) * 0.5)));
                }
                painter.add(egui::Shape::line(mids, stroke));
            }
        };
        draw_trace(
            &painter,
            &joined_raw,
            Stroke::new(1.2_f32, Color32::from_rgb(120, 176, 255)),
        );
        if let Some(blended) = &blended {
            draw_trace(
                &painter,
                blended,
                Stroke::new(1.6_f32, Color32::from_rgb(92, 255, 224)),
            );
        }
        let label_font = TextStyle::Small.resolve(ui.style());
        painter.text(
            wave_rect.left_top(),
            egui::Align2::LEFT_TOP,
            "\u{2192} loop end",
            label_font.clone(),
            Color32::from_rgb(185, 190, 204),
        );
        painter.text(
            wave_rect.right_top(),
            egui::Align2::RIGHT_TOP,
            "loop start \u{2192}",
            label_font.clone(),
            Color32::from_rgb(185, 190, 204),
        );

        // Click-risk metric on what actually plays across the jump.
        let judged = blended.as_deref().unwrap_or(&joined_raw);
        let step = (judged[n] - judged[n - 1]).abs();
        // Compare against the local per-sample motion so steady material with
        // a big jump is flagged while busy material isn't over-penalized.
        let mut local_motion = 0.0f32;
        let mut count = 0usize;
        for w in judged[n.saturating_sub(64)..(n + 64).min(judged.len())].windows(2) {
            local_motion += (w[1] - w[0]).abs();
            count += 1;
        }
        let local_motion = local_motion / count.max(1) as f32;
        let ratio = step / local_motion.max(1.0e-6);
        let (verdict, verdict_color) = if step < peak * 0.004 || ratio < 1.5 {
            ("seam OK", Color32::from_rgb(120, 220, 140))
        } else if step < peak * 0.02 || ratio < 4.0 {
            ("small step", Color32::from_rgb(240, 200, 90))
        } else {
            ("click risk", Color32::from_rgb(240, 120, 120))
        };
        let xfade_ms =
            (preview.effective_xfade_samples as f32 / preview.sample_rate.max(1) as f32) * 1000.0;
        let mode_label = if preview.effective_xfade_samples == 0 {
            "no xfade".to_string()
        } else if preview.uses_through_zero {
            format!("fade-to-0 {xfade_ms:.0} ms")
        } else {
            format!("xfade {xfade_ms:.0} ms")
        };
        let footer_y = rect.bottom() - 10.0;
        painter.text(
            egui::pos2(rect.left() + 10.0, footer_y),
            egui::Align2::LEFT_CENTER,
            format!("{verdict} (\u{0394}{step:.4})"),
            label_font.clone(),
            verdict_color,
        );
        painter.text(
            egui::pos2(rect.right() - 10.0, footer_y),
            egui::Align2::RIGHT_CENTER,
            format!("{mode_label} \u{00B7} \u{00B1}{window_ms:.0} ms"),
            label_font,
            Color32::from_rgb(150, 162, 184),
        );
    }

    /// Paint one video surface from the tab's shared frame ring and texture.
    /// Both the inline panel and the detached native viewport call this, so
    /// frame selection and letterboxing cannot drift apart.
    pub(super) fn paint_video_surface(
        ui: &egui::Ui,
        tab_id: u64,
        panel: &mut VideoPanelState,
        rect: egui::Rect,
        video_secs: f64,
        show_labels: bool,
        surface_key: &'static str,
    ) {
        use crate::app::render::video_panel;

        let painter = ui.painter();
        let panel_bg = Color32::from_rgb(16, 18, 23);
        let label_font = egui::FontId::monospace(9.0);
        let label_col = Color32::from_rgb(120, 132, 150);
        painter.rect_filled(rect, 4.0, panel_bg);

        // Pick the latest frame whose presentation time the audio clock has
        // reached. This never lets picture lead sound.
        if let Some((pts, image)) = panel.frame_at(video_secs) {
            let dims = [image.size[0], image.size[1]];
            let needs_new_texture = panel
                .texture
                .as_ref()
                .map(|tex| tex.size() != dims)
                .unwrap_or(true);
            if needs_new_texture {
                panel.texture = Some(ui.ctx().load_texture(
                    format!("video_panel_{tab_id}"),
                    (*image).clone(),
                    egui::TextureOptions::LINEAR,
                ));
                panel.shown_pts = Some(pts);
            } else if panel.shown_pts != Some(pts) {
                if let Some(tex) = panel.texture.as_mut() {
                    tex.set((*image).clone(), egui::TextureOptions::LINEAR);
                }
                panel.shown_pts = Some(pts);
            }
        }

        // If decoding falls behind, retain the last texture only while its
        // PTS is still at or behind the source clock. A backward seek never
        // flashes a future frame; switching audio sources freezes exactly the
        // last synchronized picture.
        let drawn = panel
            .texture
            .as_ref()
            .zip(panel.shown_pts)
            .filter(|(_, pts)| *pts <= video_secs)
            .map(|(tex, _)| {
                let frame_rect = video_panel::letterbox_rect(rect.shrink(2.0), panel.info.aspect());
                painter.image(
                    tex.id(),
                    frame_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
                painter.rect_stroke(
                    frame_rect,
                    0.0,
                    Stroke::new(1.0_f32, Color32::from_rgb(40, 46, 56)),
                    egui::StrokeKind::Inside,
                );
            })
            .is_some();

        if !drawn {
            let (message, color) = match &panel.status {
                VideoPanelStatus::Probing | VideoPanelStatus::Ready => {
                    if panel.seeking {
                        ("seeking...".to_string(), label_col)
                    } else {
                        ("decoding...".to_string(), label_col)
                    }
                }
                VideoPanelStatus::Unsupported(codec) => (
                    format!("no preview ({codec})"),
                    Color32::from_rgb(220, 180, 110),
                ),
                VideoPanelStatus::Failed(_) => (
                    "preview failed".to_string(),
                    Color32::from_rgb(226, 122, 122),
                ),
            };
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                message,
                label_font.clone(),
                color,
            );
        }

        if show_labels {
            painter.text(
                rect.left_top() + egui::vec2(4.0, 2.0),
                egui::Align2::LEFT_TOP,
                "VIDEO",
                label_font.clone(),
                label_col,
            );
            if let Some(pts) = panel.shown_pts {
                let mins = (pts / 60.0).floor().max(0.0) as u64;
                let secs = (pts - mins as f64 * 60.0).max(0.0);
                painter.text(
                    rect.right_bottom() + egui::vec2(-4.0, -2.0),
                    egui::Align2::RIGHT_BOTTOM,
                    format!("{mins}:{secs:05.2}"),
                    label_font,
                    label_col,
                );
            }
        }

        if let VideoPanelStatus::Failed(reason) = &panel.status {
            ui.interact(rect, egui::Id::new((surface_key, tab_id)), Sense::hover())
                .on_hover_text(reason);
        }
    }

    /// Draw the video frame column at the left of the mini meter strip and
    /// report how much width it took (including the gap after it), so the
    /// audio panels can lay out in what is left.
    ///
    /// Returns 0.0 when there is no video, or when the strip is too narrow to
    /// carry one — in which case the strip looks exactly as it did before this
    /// panel existed.
    fn draw_editor_video_panel(
        ui: &mut egui::Ui,
        tab: &mut EditorTab,
        _painter: &egui::Painter,
        inner: egui::Rect,
        gap: f32,
        video_secs: f64,
    ) -> f32 {
        use crate::app::render::video_panel;
        use crate::app::types::VideoPanelStatus;

        let tab_id = tab.tab_id;
        let Some(panel) = tab.video_panel.as_mut() else {
            return 0.0;
        };
        // A container with no picture in it (an audio-only .mp4) is an audio
        // file as far as this strip is concerned.
        if matches!(&panel.status, VideoPanelStatus::Unsupported(reason) if reason == "no video track")
        {
            return 0.0;
        }
        let aspect = panel.info.aspect();
        let column_w = video_panel::video_column_width(inner.width(), inner.height(), aspect);
        if column_w <= 0.0 {
            return 0.0;
        }
        let column = egui::Rect::from_min_size(inner.min, egui::vec2(column_w, inner.height()));
        // Tell the decoder what size to produce next. Quantized inside, so a
        // pixel of window resize does not throw away every cached frame.
        panel.wanted_box_px =
            video_panel::frame_box_px(column.shrink(2.0), aspect, ui.ctx().pixels_per_point());
        Self::paint_video_surface(
            ui,
            tab_id,
            panel,
            column,
            video_secs,
            true,
            "editor_video_panel",
        );

        let button_rect = egui::Rect::from_min_size(
            egui::pos2(column.right() - 22.0, column.top() + 2.0),
            egui::vec2(20.0, 18.0),
        );
        let response = ui
            .push_id(("video_popout_button", tab_id), |ui| {
                ui.put(button_rect, egui::Button::new("↗").small())
            })
            .inner;
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                ui.is_enabled(),
                "Open video in a separate window",
            )
        });
        let response = response.on_hover_text("Open video in a separate window");
        if response.clicked() {
            panel.popout_requested = true;
        }

        column_w + gap
    }

    /// Realtime "mini meter" strip under the editor: oscilloscope,
    /// dual-resolution spectrum analyzer, stereo vectorscope with
    /// correlation, and per-channel peak/RMS meters of the audio around
    /// the playhead. Sits on the bottom edge of the canvas column, `strip_h`
    /// tall (the splitter above it sets that; see `editor_layout`).
    /// Smoothing/hold state lives on `tab.mini_meter`.
    fn draw_editor_mini_meter(
        ui: &mut egui::Ui,
        tab: &mut EditorTab,
        strip_h: f32,
        playhead_display: usize,
        preview_audio: Option<&crate::audio::AudioBuffer>,
        preview_playhead: usize,
        preview_sample_rate: u32,
        playing: bool,
        video_secs: f64,
        spectrum_interval_secs: f64,
        layout: Option<&[Option<crate::audio_channels::SpeakerPos>]>,
        hrtf_active: bool,
    ) {
        use crate::app::render::mini_meter;

        let h = strip_h.max(lanes_layout::MINI_METER_MIN_H);
        let w = ui.available_width().max(160.0);
        let use_preview = preview_audio.filter(|audio| {
            !audio.channels.is_empty() && audio.channels.iter().any(|channel| !channel.is_empty())
        });
        // A video whose audio track is empty (or is still decoding) must still
        // show its picture, so the strip is allocated before this bails.
        let audio_meters_available = match use_preview {
            Some(audio) => !audio.channels.is_empty(),
            None => !tab.ch_samples.is_empty() && tab.samples_len > 0,
        };
        if !audio_meters_available && tab.video_panel.is_none() {
            return;
        }
        let (resp, painter) = ui.allocate_painter(egui::vec2(w, h), Sense::hover());
        let rect = resp.rect;
        painter.rect_filled(rect, 6.0, Color32::from_rgb(11, 12, 16));
        painter.rect_stroke(
            rect,
            6.0,
            Stroke::new(1.0_f32, Color32::from_rgb(40, 46, 56)),
            egui::StrokeKind::Outside,
        );

        // ---- Video column ------------------------------------------------
        // Drawn first, and taken off the left of the strip, so the existing
        // four-panel arithmetic below runs unchanged on what is left. That is
        // also what gives the strip the right priority for free: SCOPE tests
        // the *remaining* width against its own threshold, so it is the panel
        // that yields when the two no longer fit together.
        let strip_pad = 6.0;
        let strip_gap = 8.0;
        let full_inner = rect.shrink(strip_pad);
        let video_reserved_w =
            Self::draw_editor_video_panel(ui, tab, &painter, full_inner, strip_gap, video_secs);

        // Bound after the video panel so its `&mut tab` does not collide with
        // the borrow of `tab.ch_samples` this holds for the rest of the strip.
        let (meter_channels, meter_playhead, sr, meter_len_limit) = if let Some(audio) = use_preview
        {
            (
                audio.channels.as_slice(),
                preview_playhead,
                preview_sample_rate.max(1),
                usize::MAX,
            )
        } else {
            (
                tab.ch_samples.as_slice(),
                playhead_display,
                tab.buffer_sample_rate.max(1),
                tab.samples_len,
            )
        };
        let srf = sr as f32;
        let n_ch = meter_channels.len();
        let buf_len = meter_channels
            .iter()
            .map(|c| c.len())
            .min()
            .unwrap_or(0)
            .min(meter_len_limit);
        if buf_len == 0 {
            return;
        }
        let end = meter_playhead.clamp(1, buf_len);
        // Frame timing for meter ballistics. The FFT cadence is tiered; when
        // it is not due we also avoid building its much longer mono history.
        let now = ui.input(|i| i.time);
        let spectrum_time_due = tab.mini_meter.spectrum_db.is_empty()
            || now - tab.mini_meter.spectrum_last_time >= spectrum_interval_secs;
        let spec_n = mini_meter::spectrum_history_len(sr).min(buf_len);
        let meter_window = |secs: f64, max: usize| {
            crate::sample_rate::frames_for_secs(secs, sr)
                .clamp(MINI_METER_MIN_WINDOW_FRAMES, max)
                .min(buf_len)
        };
        let scope_n = meter_window(MINI_SCOPE_WINDOW_SECS, MINI_METER_MAX_WINDOW_FRAMES);
        let level_n = meter_window(MINI_LEVEL_WINDOW_SECS, MINI_METER_MAX_WINDOW_FRAMES);
        let vector_n = meter_window(MINI_VECTOR_WINDOW_SECS, MINI_VECTOR_MAX_WINDOW_FRAMES);
        let spectrum_need = if spectrum_time_due { spec_n } else { 0 };
        let need = spectrum_need.max(scope_n).max(level_n).min(end);
        let start = end - need;

        let dt = ((now - tab.mini_meter.last_time).max(0.0) as f32).min(0.25);
        tab.mini_meter.last_time = now;

        // Mono mixdown + spectrum target reuse thread-local scratch to
        // stay allocation-free at 30 fps.
        thread_local! {
            static METER_SCRATCH: std::cell::RefCell<(Vec<f32>, Vec<f32>)> =
                const { std::cell::RefCell::new((Vec::new(), Vec::new())) };
        }

        let pad = strip_pad;
        let gap = strip_gap;
        // Everything below lays out inside what the video column left behind.
        let inner = {
            let full = rect.shrink(pad);
            egui::Rect::from_min_max(
                egui::pos2(full.left() + video_reserved_w, full.top()),
                full.max,
            )
        };
        if inner.width() < 40.0 {
            return;
        }
        let label_font = egui::FontId::monospace(9.0);
        let label_col = Color32::from_rgb(120, 132, 150);
        let panel_bg = Color32::from_rgb(16, 18, 23);

        // ---- Layout: [SCOPE][SPECTRUM][STEREO][LEVELS] -------------------
        // `min` then `max`, never `clamp`: under 200 px of strip the upper
        // bound falls below the lower one, and `f32::clamp` panics on that.
        let levels_w = (30.0 + n_ch as f32 * 14.0)
            .min(inner.width() * 0.28)
            .max(56.0);
        let stereo_w = inner
            .height()
            .clamp(56.0, 170.0)
            .min((inner.width() * 0.24).max(56.0));
        let show_scope = inner.width() >= 430.0;
        let scope_w = if show_scope {
            ((inner.width() - levels_w - stereo_w - gap * 3.0) * 0.30).max(60.0)
        } else {
            0.0
        };
        let used = levels_w + stereo_w + scope_w + gap * (if show_scope { 3.0 } else { 2.0 });
        let spectrum_w = (inner.width() - used).max(40.0);
        let scope_rect = egui::Rect::from_min_size(inner.min, egui::vec2(scope_w, inner.height()));
        let spectrum_left = if show_scope {
            scope_rect.right() + gap
        } else {
            inner.left()
        };
        let spectrum_rect = egui::Rect::from_min_size(
            egui::pos2(spectrum_left, inner.top()),
            egui::vec2(spectrum_w, inner.height()),
        );
        let stereo_rect = egui::Rect::from_min_size(
            egui::pos2(spectrum_rect.right() + gap, inner.top()),
            egui::vec2(stereo_w, inner.height()),
        );
        let levels_rect = egui::Rect::from_min_size(
            egui::pos2(stereo_rect.right() + gap, inner.top()),
            egui::vec2(levels_w, inner.height()),
        );

        METER_SCRATCH.with(|cell| {
            let mut scratch = cell.borrow_mut();
            let (mono, spectrum_target) = &mut *scratch;
            mono.clear();
            mono.reserve(need);
            let inv_ch = 1.0 / n_ch.max(1) as f32;
            mono.extend((start..end).map(|i| {
                meter_channels
                    .iter()
                    .map(|c| c.get(i).copied().unwrap_or(0.0))
                    .sum::<f32>()
                    * inv_ch
            }));

            // ---- Oscilloscope ---------------------------------------------
            if show_scope {
                painter.rect_filled(scope_rect, 4.0, panel_bg);
                let scope_take = scope_n.min(mono.len());
                if scope_take >= 2 {
                    let scope = &mono[mono.len() - scope_take..];
                    let peak = scope.iter().fold(0.0f32, |a, v| a.max(v.abs()));
                    let gain = (0.85 / peak.max(1.0e-4)).min(24.0);
                    let mid_y = scope_rect.center().y;
                    let denom = (scope_take - 1) as f32;
                    let mut last: Option<egui::Pos2> = None;
                    let step = (scope_take as f32 / scope_rect.width().max(1.0)).max(1.0);
                    let mut i = 0.0f32;
                    while (i as usize) < scope_take {
                        let idx = i as usize;
                        let v = (scope[idx] * gain).clamp(-1.0, 1.0);
                        let pt = egui::pos2(
                            egui::lerp(scope_rect.x_range(), idx as f32 / denom),
                            mid_y - v * scope_rect.height() * 0.48,
                        );
                        if let Some(prev) = last {
                            let heat = v.abs().powf(0.6);
                            painter.line_segment(
                                [prev, pt],
                                Stroke::new(
                                    1.2_f32,
                                    crate::app::helpers::lerp_color(
                                        Color32::from_rgb(96, 200, 255),
                                        Color32::from_rgb(255, 96, 128),
                                        heat,
                                    ),
                                ),
                            );
                        }
                        last = Some(pt);
                        i += step;
                    }
                }
                painter.text(
                    scope_rect.left_top() + egui::vec2(4.0, 2.0),
                    egui::Align2::LEFT_TOP,
                    "SCOPE",
                    label_font.clone(),
                    label_col,
                );
            }

            // ---- Spectrum ---------------------------------------------------
            painter.rect_filled(spectrum_rect, 4.0, panel_bg);
            tab.mini_meter.spectrum_rect = Some(spectrum_rect);
            let db_floor = mini_meter::SPECTRUM_DB_FLOOR;
            let cols = (spectrum_rect.width().max(8.0) as usize).min(1_024);
            let spectrum_due = tab.mini_meter.spectrum_db.len() != cols || spectrum_time_due;
            if spectrum_due {
                let spectrum_dt =
                    ((now - tab.mini_meter.spectrum_last_time).max(0.0) as f32).min(0.25);
                mini_meter::spectrum_columns(mono, sr, cols, spectrum_target);
                mini_meter::smooth_spectrum_db(
                    &mut tab.mini_meter.spectrum_db,
                    spectrum_target,
                    spectrum_dt,
                );
                tab.mini_meter.spectrum_last_time = now;
            }
            // Faint dB grid lines.
            for db in [-24.0f32, -48.0, -72.0] {
                let t = (db - db_floor) / -db_floor;
                let y = spectrum_rect.bottom() - t * (spectrum_rect.height() - 12.0);
                painter.line_segment(
                    [
                        egui::pos2(spectrum_rect.left() + 1.0, y),
                        egui::pos2(spectrum_rect.right() - 1.0, y),
                    ],
                    Stroke::new(1.0_f32, Color32::from_rgb(26, 29, 36)),
                );
            }
            // One mesh, not a rect per column: abutting anti-aliased rects
            // left dark seams at fractional display scales (see
            // `render::spectrum_fill`). Hue sweeps blue -> red across the
            // log-frequency axis.
            painter.add(crate::app::render::spectrum_fill::spectrum_mesh(
                spectrum_rect,
                &tab.mini_meter.spectrum_db,
                db_floor,
                12.0,
                |t, norm| egui::ecolor::Hsva::new(0.62 - 0.62 * t, 0.78, 0.55 + 0.45 * norm, 1.0).into(),
            ));
            let nyquist = srf * 0.5;
            let f_lo = mini_meter::SPECTRUM_MIN_HZ.min(nyquist * 0.25);
            let f_hi = nyquist.max(f_lo * 2.0);
            for (freq, label) in [
                (50.0f32, "50"),
                (100.0, "100"),
                (1_000.0, "1k"),
                (10_000.0, "10k"),
            ] {
                if freq >= f_lo && freq <= f_hi {
                    let t = (freq / f_lo).ln() / (f_hi / f_lo).ln();
                    let x = spectrum_rect.left() + t * spectrum_rect.width();
                    painter.line_segment(
                        [
                            egui::pos2(x, spectrum_rect.bottom() - 4.0),
                            egui::pos2(x, spectrum_rect.bottom()),
                        ],
                        Stroke::new(1.0_f32, label_col),
                    );
                    painter.text(
                        egui::pos2(x + 2.0, spectrum_rect.bottom() - 4.0),
                        egui::Align2::LEFT_BOTTOM,
                        label,
                        label_font.clone(),
                        label_col,
                    );
                }
            }
            painter.text(
                spectrum_rect.left_top() + egui::vec2(4.0, 2.0),
                egui::Align2::LEFT_TOP,
                "SPECTRUM",
                label_font.clone(),
                label_col,
            );
        });

        // ---- Stereo vectorscope + correlation, or the SURROUND view --------
        painter.rect_filled(stereo_rect, 4.0, panel_bg);
        let surround_layout = layout
            .filter(|layout| layout.len() == n_ch && super::surround_meter::wants_surround(layout));
        tab.mini_meter.channel_view = if surround_layout.is_some() {
            "SURROUND"
        } else {
            "STEREO"
        };
        if let Some(layout) = surround_layout {
            let take = level_n.min(end);
            super::surround_meter::draw_surround(
                &painter,
                stereo_rect,
                meter_channels,
                end - take,
                end,
                layout,
                &mut tab.mini_meter,
                &label_font,
                label_col,
            );
        } else {
        {
            let corr_h = 10.0;
            let scope_area = egui::Rect::from_min_max(
                stereo_rect.left_top() + egui::vec2(2.0, 11.0),
                egui::pos2(
                    stereo_rect.right() - 2.0,
                    stereo_rect.bottom() - corr_h - 4.0,
                ),
            );
            let radius = (scope_area.width().min(scope_area.height()) * 0.5 - 2.0).max(8.0);
            let center = scope_area.center();
            let guide = Stroke::new(1.0_f32, Color32::from_rgb(34, 39, 48));
            painter.circle_stroke(center, radius, guide);
            let diag = radius * std::f32::consts::FRAC_1_SQRT_2;
            // L / R axes (45 degrees).
            painter.line_segment(
                [
                    egui::pos2(center.x - diag, center.y - diag),
                    egui::pos2(center.x + diag, center.y + diag),
                ],
                guide,
            );
            painter.line_segment(
                [
                    egui::pos2(center.x + diag, center.y - diag),
                    egui::pos2(center.x - diag, center.y + diag),
                ],
                guide,
            );
            painter.text(
                egui::pos2(center.x - diag - 2.0, center.y - diag - 2.0),
                egui::Align2::RIGHT_BOTTOM,
                "L",
                label_font.clone(),
                label_col,
            );
            painter.text(
                egui::pos2(center.x + diag + 2.0, center.y - diag - 2.0),
                egui::Align2::LEFT_BOTTOM,
                "R",
                label_font.clone(),
                label_col,
            );

            // Channel pairing: stereo uses L/R; mono duplicates the single
            // channel (trace collapses onto the mid axis); multichannel
            // uses the first pair (front L/R in standard layouts) — the
            // remaining channels are covered by the per-channel meters.
            let (li, ri) = if n_ch >= 2 {
                (0usize, 1usize)
            } else {
                (0usize, 0usize)
            };
            let take = vector_n.min(end);
            let l_win = &meter_channels[li][end - take..end];
            let r_win = &meter_channels[ri][end - take..end];
            let trace = Color32::from_rgba_unmultiplied(96, 220, 200, 110);
            // Same allocation-free scratch pattern as METER_SCRATCH above.
            thread_local! {
                static GONIO_SCRATCH: std::cell::RefCell<Vec<(f32, f32)>> =
                    const { std::cell::RefCell::new(Vec::new()) };
            }
            GONIO_SCRATCH.with_borrow_mut(|pts| {
                crate::app::render::mini_meter::goniometer_points_into(l_win, r_win, 480, pts);
                let mut last: Option<egui::Pos2> = None;
                for &(x, y) in pts.iter() {
                    let pt = egui::pos2(center.x + x * radius, center.y - y * radius);
                    if let Some(prev) = last {
                        painter.line_segment([prev, pt], Stroke::new(1.0_f32, trace));
                    }
                    last = Some(pt);
                }
            });

            // Correlation bar along the bottom (-1 .. +1).
            let target_corr = if n_ch >= 2 {
                let take = level_n.min(end);
                crate::app::render::mini_meter::stereo_correlation(
                    &meter_channels[0][end - take..end],
                    &meter_channels[1][end - take..end],
                )
            } else {
                1.0
            };
            let k = 1.0 - (-dt / 0.25f32).exp();
            tab.mini_meter.corr += (target_corr - tab.mini_meter.corr) * k;
            let corr = tab.mini_meter.corr.clamp(-1.0, 1.0);
            let bar = egui::Rect::from_min_max(
                egui::pos2(
                    stereo_rect.left() + 6.0,
                    stereo_rect.bottom() - corr_h - 2.0,
                ),
                egui::pos2(stereo_rect.right() - 6.0, stereo_rect.bottom() - 4.0),
            );
            painter.rect_filled(bar, 2.0, Color32::from_rgb(24, 27, 33));
            let cx = bar.center().x;
            let fill_col = if corr >= 0.0 {
                crate::app::helpers::lerp_color(
                    Color32::from_rgb(235, 200, 90),
                    Color32::from_rgb(88, 200, 120),
                    corr,
                )
            } else {
                crate::app::helpers::lerp_color(
                    Color32::from_rgb(235, 200, 90),
                    Color32::from_rgb(240, 100, 100),
                    -corr,
                )
            };
            let fx = cx + corr * (bar.width() * 0.5 - 1.0);
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(fx.min(cx), bar.top() + 1.0),
                    egui::pos2(fx.max(cx), bar.bottom() - 1.0),
                ),
                1.0,
                fill_col,
            );
            painter.line_segment(
                [egui::pos2(cx, bar.top()), egui::pos2(cx, bar.bottom())],
                Stroke::new(1.0_f32, Color32::from_rgb(70, 78, 92)),
            );
            painter.text(
                egui::pos2(bar.left() - 1.0, bar.center().y),
                egui::Align2::RIGHT_CENTER,
                "-",
                label_font.clone(),
                label_col,
            );
            painter.text(
                egui::pos2(bar.right() + 1.0, bar.center().y),
                egui::Align2::LEFT_CENTER,
                "+",
                label_font.clone(),
                label_col,
            );

            // Title row: the pane name on the left, then the channel badge and
            // the numeric correlation packed in from the right.
            //
            // Measured rather than estimated. This font is nominally
            // `monospace`, but `theme_ops::init_egui_style` puts the bundled
            // CJK face at the head of the Monospace family and the system font
            // upgrade swaps it again while the app is running, so the ASCII
            // glyphs here are proportional and their widths change mid-session.
            // A per-character width would be wrong from the start and wrong in
            // a different way later.
            let label_top = stereo_rect.top() + 2.0;
            let title = painter.layout_no_wrap("STEREO".to_string(), label_font.clone(), label_col);
            let title_right = stereo_rect.left() + 4.0 + title.size().x;
            painter.galley(
                egui::pos2(stereo_rect.left() + 4.0, label_top),
                title,
                label_col,
            );
            let mut right_x = stereo_rect.right() - 4.0;
            let mut place_right = |text: String, color: Color32| {
                let galley = painter.layout_no_wrap(text, label_font.clone(), color);
                let left = right_x - galley.size().x;
                // Anything that would reach the title is dropped instead of
                // drawn over it: the pane floors at 56 px wide, which is not
                // always enough for all three.
                if left < title_right + 4.0 {
                    return;
                }
                painter.galley(egui::pos2(left, label_top), galley, color);
                right_x = left - 4.0;
            };
            let badge_col = Color32::from_rgb(150, 162, 184);
            if n_ch == 1 {
                place_right("MONO".to_string(), badge_col);
            } else if n_ch > 2 {
                place_right("CH1+2".to_string(), badge_col);
            }
            // Same smoothed value as the bar below. Mono has no correlation to
            // report -- `target_corr` is hard-wired to 1.0 up there -- and a
            // constant `+1.00` under the MONO badge is what used to render the
            // two on top of each other.
            if n_ch >= 2 {
                place_right(format!("{corr:+.2}"), fill_col);
            }
        }
        }

        // ---- Per-channel peak / RMS meters ---------------------------------
        painter.rect_filled(levels_rect, 4.0, panel_bg);
        let db_of = |v: f32| 20.0 * v.max(1.0e-6).log10();
        let meter_floor = -60.0f32;
        let norm_of = |db: f32| ((db - meter_floor) / -meter_floor).clamp(0.0, 1.0);
        if tab.mini_meter.peak_hold_db.len() != n_ch {
            tab.mini_meter.peak_hold_db.clear();
            tab.mini_meter.peak_hold_db.resize(n_ch, meter_floor);
        }
        let scale_w = 22.0;
        // With more than two channels the names under the bars meet the
        // peak readout at the bottom; a wide enough panel moves it up beside
        // the title, and the bars start a little lower to clear it.
        let readout_on_top = n_ch > 2 && levels_rect.width() >= 90.0;
        let bars_top = if readout_on_top { 18.0 } else { 14.0 };
        // Names under the bars and the readout below them each need a row.
        let bars_bottom = if !readout_on_top && n_ch > 1 { 25.0 } else { 16.0 };
        let bars_rect = egui::Rect::from_min_max(
            egui::pos2(levels_rect.left() + 6.0, levels_rect.top() + bars_top),
            egui::pos2(
                (levels_rect.right() - scale_w).max(levels_rect.left() + 10.0),
                levels_rect.bottom() - bars_bottom,
            ),
        );
        for db in [0.0f32, -6.0, -18.0, -36.0] {
            let y = bars_rect.bottom() - norm_of(db) * bars_rect.height();
            painter.text(
                egui::pos2(bars_rect.right() + 4.0, y),
                egui::Align2::LEFT_CENTER,
                format!("{db:.0}"),
                label_font.clone(),
                label_col,
            );
        }
        let slot_w = bars_rect.width() / n_ch.max(1) as f32;
        let bar_w = (slot_w - 2.0).clamp(2.0, 18.0);
        let show_ch_labels = slot_w >= 9.0 && n_ch > 1;
        let mut max_peak_db = meter_floor;
        for c in 0..n_ch {
            let take = level_n.min(end);
            let win = &meter_channels[c][end - take..end];
            let peak = win.iter().fold(0.0f32, |a, v| a.max(v.abs()));
            let rms = (win.iter().map(|v| v * v).sum::<f32>() / take.max(1) as f32).sqrt();
            let peak_db = db_of(peak);
            max_peak_db = max_peak_db.max(peak_db);
            let hold = &mut tab.mini_meter.peak_hold_db[c];
            *hold = (*hold - dt * 12.0).max(peak_db).max(meter_floor);
            let x0 = bars_rect.left() + c as f32 * slot_w + (slot_w - bar_w) * 0.5;
            let bar_rect = egui::Rect::from_min_max(
                egui::pos2(x0, bars_rect.top()),
                egui::pos2(x0 + bar_w, bars_rect.bottom()),
            );
            painter.rect_filled(bar_rect, 2.0, Color32::from_rgb(24, 27, 33));
            let zone = |lo_db: f32, hi_db: f32, color: Color32| {
                let lo = norm_of(lo_db);
                let hi = norm_of(hi_db.min(peak_db));
                if hi > lo {
                    painter.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(bar_rect.left(), bar_rect.bottom() - hi * bar_rect.height()),
                            egui::pos2(
                                bar_rect.right(),
                                bar_rect.bottom() - lo * bar_rect.height(),
                            ),
                        ),
                        1.0,
                        color,
                    );
                }
            };
            zone(meter_floor, -18.0, Color32::from_rgb(88, 200, 120));
            zone(-18.0, -6.0, Color32::from_rgb(235, 200, 90));
            zone(-6.0, 0.0, Color32::from_rgb(240, 100, 100));
            let rms_y = bar_rect.bottom() - norm_of(db_of(rms)) * bar_rect.height();
            painter.line_segment(
                [
                    egui::pos2(bar_rect.left(), rms_y),
                    egui::pos2(bar_rect.right(), rms_y),
                ],
                Stroke::new(1.5_f32, Color32::WHITE),
            );
            let hold_y = bar_rect.bottom() - norm_of(*hold) * bar_rect.height();
            painter.line_segment(
                [
                    egui::pos2(bar_rect.left(), hold_y),
                    egui::pos2(bar_rect.right(), hold_y),
                ],
                Stroke::new(1.0_f32, Color32::from_rgb(255, 196, 72)),
            );
            if show_ch_labels {
                // The speaker the channel feeds, when its layout says.
                let named = layout
                    .filter(|layout| layout.len() == n_ch)
                    .and_then(|layout| layout[c].map(|pos| pos.label(layout).to_string()));
                let label = named.unwrap_or_else(|| match (n_ch, c) {
                    (2, 0) => "L".to_string(),
                    (2, 1) => "R".to_string(),
                    _ => format!("{}", c + 1),
                });
                // "Ltf" under a 14 px bar needs the smaller face; anything
                // that still does not fit its slot is left out, not overlapped.
                let mut galley = painter.layout_no_wrap(label.clone(), label_font.clone(), label_col);
                if galley.size().x > slot_w {
                    galley = painter.layout_no_wrap(label, egui::FontId::monospace(7.0), label_col);
                }
                if galley.size().x <= slot_w + 1.0 {
                    let at = egui::pos2(bar_rect.center().x - galley.size().x * 0.5, bars_rect.bottom() + 1.0);
                    painter.galley(at, galley, label_col);
                }
            }
        }
        painter.text(
            levels_rect.left_top() + egui::vec2(4.0, 2.0),
            egui::Align2::LEFT_TOP,
            "PEAK",
            label_font.clone(),
            label_col,
        );
        // Right-click the channel views to say which speaker each channel is.
        ui.interact(
            stereo_rect.union(levels_rect),
            ui.id().with("mini_meter_channel_layout"),
            Sense::click(),
        )
        .on_hover_text("Right-click: channel layout, virtual speakers (HRTF)")
        .context_menu(|ui| {
            if ui.button("Channel layout\u{2026}").clicked() {
                tab.mini_meter.layout_editor_requested = true;
                ui.close();
            }
            if ui.button("Virtual speakers (HRTF)\u{2026}").clicked() {
                tab.mini_meter.hrtf_window_requested = true;
                ui.close();
            }
        });
        // Say so when what is heard is the headphone rendering, not this view
        // folded to stereo.
        if hrtf_active {
            painter.text(
                stereo_rect.right_top() + egui::vec2(-4.0, 2.0),
                egui::Align2::RIGHT_TOP,
                "HRTF",
                label_font.clone(),
                ui.visuals().selection.bg_fill,
            );
        }
        let (readout_at, readout_align) = if readout_on_top {
            (egui::pos2(levels_rect.right() - 4.0, levels_rect.top() + 2.0), egui::Align2::RIGHT_TOP)
        } else {
            (egui::pos2(levels_rect.center().x, levels_rect.bottom() - 2.0), egui::Align2::CENTER_BOTTOM)
        };
        painter.text(
            readout_at,
            readout_align,
            if max_peak_db <= meter_floor + 0.5 {
                "-inf dB".to_string()
            } else {
                format!("{max_peak_db:+.1} dB")
            },
            label_font,
            Color32::from_rgb(200, 210, 224),
        );

        // Keep animating while playing or while decay is still visible so
        // bars settle back to the floor after the sound stops.
        let db_floor = crate::app::render::mini_meter::SPECTRUM_DB_FLOOR;
        let decaying = tab
            .mini_meter
            .spectrum_db
            .iter()
            .any(|db| *db > db_floor + 0.5)
            || tab
                .mini_meter
                .peak_hold_db
                .iter()
                .any(|db| *db > meter_floor + 0.5);
        tab.mini_meter.active = playing || decaying;
        if tab.mini_meter.active {
            ui.ctx()
                .request_repaint_after(crate::app::ui_timing::SMOOTH_REFRESH);
        }
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
        let sample_point_radius = Self::waveform_sample_point_radius(spp);
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
        let samples_per_column = visible_len as f32 / bins as f32;
        let pyramid_base_samples = tab
            .waveform_pyramid
            .as_ref()
            .map(|set| set.base_bin_samples.max(1) as f32)
            .unwrap_or(f32::INFINITY);
        let mut lod = waveform_render_lod(samples_per_column, pyramid_base_samples);

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
                                egui::Stroke::new(1.0_f32, col),
                            ));
                        }
                        if let Some(radius) = sample_point_radius {
                            for (point, &sample) in line_points.iter().zip(mono.iter()) {
                                let v = (sample * scale).clamp(-1.0, 1.0);
                                let col = amp_to_color(v.abs().clamp(0.0, 1.0));
                                shapes.push(egui::Shape::circle_filled(*point, radius, col));
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
                                egui::Stroke::new(1.0_f32, col),
                            ));
                        }
                        if let Some(radius) = sample_point_radius {
                            for (point, &sample) in line_points.iter().zip(samples.iter()) {
                                let v = (sample * scale).clamp(-1.0, 1.0);
                                let col = amp_to_color(v.abs().clamp(0.0, 1.0));
                                shapes.push(egui::Shape::circle_filled(*point, radius, col));
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

    /// Show explicit sample handles once neighboring samples have enough
    /// horizontal separation to remain individually readable.
    fn waveform_sample_point_radius(samples_per_px: f32) -> Option<f32> {
        let px_per_sample = 1.0 / samples_per_px.max(1.0e-6);
        (px_per_sample >= 2.0).then_some((px_per_sample * 0.18).clamp(1.15, 2.25))
    }

    pub(in crate::app) fn ui_editor_view(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        tab_idx: usize,
    ) {
        let editor_scroll_active = self.ui_input_focus.is_active(UiSurface::Editor);
        let editor_scroll_source = self.scroll_source_for(UiSurface::Editor);
        let mini_meter_spectrum_interval_secs =
            self.perf.mini_meter_spectrum_interval().as_secs_f64();
        // Object audio's samples are read-only (`media_kind`). The list's
        // metadata can arrive after the tab opened, so this is checked here
        // as well as on open -- a lookup, not a read.
        if !self.tabs[tab_idx].read_only && self.source_content(&self.tabs[tab_idx].path).object_audio {
            self.tabs[tab_idx].read_only = true;
        }
        if self.tabs[tab_idx].primary_view == EditorPrimaryView::Metadata {
            self.ui_metadata_inspector(ui, ctx, tab_idx);
            return;
        }
        if self.tabs[tab_idx].primary_view == EditorPrimaryView::Spatial {
            self.ui_spatial_view(ui, ctx, tab_idx);
            return;
        }
        let editor_panel_rect = ui.max_rect();
        let mut apply_pending_loop = false;
        let mut save_spectro_color_range = false;
        // Double-clicking a loop handle zooms in around that point. Deferred
        // because the zoom helpers take `&mut self` and the pointer block runs
        // with the tab already borrowed.
        let mut zoom_around_loop_point: Option<usize> = None;
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
            ViewMode::World => Some(EditorAnalysisKind::World),
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
        let world_feature_cache = match feature_cache.as_deref() {
            Some(EditorFeatureAnalysisData::World(_)) => feature_cache.clone(),
            _ => None,
        };
        let world_data = world_feature_cache.as_deref().and_then(|data| match data {
            EditorFeatureAnalysisData::World(data) => Some(data.as_ref()),
            _ => None,
        });
        let mut touch_spectro_cache = false;
        // (source seconds, transport running) for this tab's video panel, acted
        // on after the body so the request can reach `self`.
        let mut video_request: Option<(f64, bool)> = None;
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
            if tab.read_only {
                ui.add(
                    egui::Label::new(
                        RichText::new("READ-ONLY")
                            .small()
                            .monospace()
                            .color(Color32::from_rgb(220, 180, 110)),
                    )
                    .sense(egui::Sense::hover()),
                )
                .on_hover_text(if tab.audio_track_unsupported {
                    format!(
                        "{} The picture remains playable and seekable on a silent timeline, and there is no video encoder to write changes back out either.",
                        tab.unsupported_audio_reason()
                    )
                } else if tab.audio_track_absent {
                    "This is a video file with no audio track. It remains playable and seekable on a silent timeline, but NeoWaves has no video encoder — so it cannot be edited or written back out.".to_string()
                } else {
                    "This is a video file. Its audio track plays, measures and previews like any other source, but NeoWaves has no video encoder — so it cannot be edited or written back out.".to_string()
                });
            }
            if tab.uses_silent_video_timeline() {
                ui.add(
                    egui::Label::new(
                        RichText::new(if tab.audio_track_unsupported {
                            tab.unsupported_audio_label()
                        } else {
                            "NO AUDIO".to_string()
                        })
                            .small()
                            .monospace()
                            .color(Color32::from_rgb(220, 180, 110)),
                    )
                    .sense(egui::Sense::hover()),
                )
                .on_hover_text(if tab.audio_track_unsupported {
                    format!(
                        "{} Playback and seeking use a silent timeline so the picture remains fully previewable.",
                        tab.unsupported_audio_reason()
                    )
                } else {
                    "This video has no audio track. Playback and seeking use a silent timeline so the picture remains fully previewable.".to_string()
                });
            }
            if tab.paged_asset {
                ui.add(
                    egui::Label::new(
                        RichText::new("OVERVIEW ONLY")
                            .small()
                            .monospace()
                            .color(Color32::from_rgb(220, 180, 110)),
                    )
                    .sense(egui::Sense::hover()),
                )
                .on_hover_text(
                    "This file decodes to more than the editor's resident buffer holds, so only the whole-file overview is loaded. The timeline, playback and seeking work; editing tools need the full buffer and stay disabled. Raise NEOWAVES_MAX_RESIDENT_DECODE_BYTES to load it in full.",
                );
            }
        });
        let mut discard_preview_for_view_change = false;
        let mut request_preview_refresh = false;
        let mut requested_channel_view: Option<ChannelView> = None;
        let channel_count = self.tabs[tab_idx].ch_samples.len();
        let tab_is_object_audio = self.source_content(&self.tabs[tab_idx].path).object_audio;
        // Undo / Redo live with the edit they act on, not inside a tool's
        // panel. The tooltip names the step, as the History window does.
        let history_step = |redo: bool| -> Option<String> {
            let tab = self.tabs.get(tab_idx)?;
            if let Some(session_step) = tab
                .tool_session
                .has_draft_history(redo)
                .then(|| format!("{} setting", tab.active_tool.label()))
            {
                return Some(session_step);
            }
            let stack = if redo { &tab.redo_stack } else { &tab.undo_stack };
            stack.last().map(|state| state.label.clone())
        };
        let undo_step = self.undo_redo_available(false).then(|| history_step(false)).flatten();
        let redo_step = self.undo_redo_available(true).then(|| history_step(true)).flatten();
        let mut toolbar_history: Option<bool> = None;
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(undo_step.is_some(), egui::Button::new("⟲").small())
                .on_hover_text(match undo_step.as_deref() {
                    Some(step) => format!("Undo {step} (Ctrl+Z)"),
                    None => "Nothing to undo".to_string(),
                })
                .clicked()
            {
                toolbar_history = Some(false);
            }
            if ui
                .add_enabled(redo_step.is_some(), egui::Button::new("⟳").small())
                .on_hover_text(match redo_step.as_deref() {
                    Some(step) => format!("Redo {step} (Ctrl+Y / Ctrl+Shift+Z)"),
                    None => "Nothing to redo".to_string(),
                })
                .clicked()
            {
                toolbar_history = Some(true);
            }
            ui.separator();
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
                        ui.selectable_value(&mut selected_view, ViewMode::World, "World (F0/Env)");
                        ui.separator();
                        if ui.selectable_label(false, "Metadata").clicked() {
                            tab.primary_view = EditorPrimaryView::Metadata;
                        }
                        if tab_is_object_audio
                            && ui
                                .selectable_label(false, "Spatial")
                                .on_hover_text("Where every bed and object is heard, and when")
                                .clicked()
                        {
                            tab.primary_view = EditorPrimaryView::Spatial;
                        }
                    });
            });
            if selected_view != prev_view {
                if tab.selection_stretch_gesture.take().is_some()
                    && ui.input(|input| input.pointer.primary_down())
                {
                    tab.selection_stretch_cancel_until_release = true;
                }
                tab.selection_edge_drag_anchor = None;
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
                let channel_menu_max_h = (ctx.content_rect().height() * 0.6).max(160.0);
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
                ui.menu_button("Channels", |ui| {
                    let mut selection_changed = false;
                    // A file with hundreds of channels would run the list off
                    // the screen, and "Clear" with it.
                    egui::ScrollArea::vertical()
                        .id_salt("editor_channel_view_list")
                        .max_height(channel_menu_max_h)
                        .show(ui, |ui| {
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
                        });
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
                // Playback-only channel mute/solo (mapping in the audio
                // callback; not an edit, so not undoable and never saved).
                let ms_active = tab.ch_muted.iter().any(|&m| m) || tab.ch_solo.iter().any(|&s| s);
                let ms_label = if ms_active { "M/S*" } else { "M/S" };
                ui.menu_button(ms_label, |ui| {
                    tab.ch_muted.resize(channel_count, false);
                    tab.ch_solo.resize(channel_count, false);
                    ui.label("Playback mute / solo");
                    egui::ScrollArea::vertical()
                        .id_salt("editor_channel_mute_solo_list")
                        .max_height(channel_menu_max_h)
                        .show(ui, |ui| {
                            for idx in 0..channel_count {
                                ui.horizontal(|ui| {
                                    let name = match (channel_count, idx) {
                                        (2, 0) => "L".to_string(),
                                        (2, 1) => "R".to_string(),
                                        _ => format!("Ch {}", idx + 1),
                                    };
                                    ui.label(name);
                                    if ui
                                        .selectable_label(tab.ch_muted[idx], "M")
                                        .on_hover_text("Mute this channel during playback")
                                        .clicked()
                                    {
                                        tab.ch_muted[idx] = !tab.ch_muted[idx];
                                    }
                                    if ui
                                        .selectable_label(tab.ch_solo[idx], "S")
                                        .on_hover_text("Solo this channel during playback")
                                        .clicked()
                                    {
                                        tab.ch_solo[idx] = !tab.ch_solo[idx];
                                    }
                                });
                            }
                        });
                    if ui.button("Clear").clicked() {
                        tab.ch_muted.iter_mut().for_each(|m| *m = false);
                        tab.ch_solo.iter_mut().for_each(|s| *s = false);
                    }
                })
                .response
                .on_hover_text("Per-channel playback mute/solo (monitoring only)");
            }
            ui.separator();
            ui.menu_button("Grid", |ui| {
                let mut bpm_enabled = tab.bpm_enabled;
                if ui.checkbox(&mut bpm_enabled, "Show BPM grid").changed() {
                    tab.bpm_enabled = bpm_enabled;
                }
                ui.horizontal(|ui| {
                    ui.label("BPM");
                    let mut bpm_value = tab.bpm_value;
                    let bpm_resp = ui.add(
                        egui::DragValue::new(&mut bpm_value)
                            .range(0.0..=300.0)
                            .speed(0.1)
                            .fixed_decimals(2),
                    );
                    if bpm_resp.changed() {
                        tab.bpm_value = bpm_value.max(0.0);
                        tab.bpm_user_set = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Offset");
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
                });
                ui.horizontal(|ui| {
                    ui.label("Time signature");
                    let mut tsig_num = tab.time_sig_numerator as f64;
                    let mut tsig_den = tab.time_sig_denominator as f64;
                    let num_resp = ui.add(
                        egui::DragValue::new(&mut tsig_num)
                            .range(1.0..=64.0)
                            .speed(1.0)
                            .fixed_decimals(0),
                    );
                    ui.label("/");
                    let den_resp = ui.add(
                        egui::DragValue::new(&mut tsig_den)
                            .range(1.0..=64.0)
                            .speed(1.0)
                            .fixed_decimals(0),
                    );
                    if num_resp.changed() {
                        tab.time_sig_numerator = (tsig_num as u8).max(1);
                    }
                    if den_resp.changed() {
                        tab.time_sig_denominator = (tsig_den as u8).max(1);
                    }
                });
            });
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
        if let Some(redo) = toolbar_history {
            self.trigger_undo_redo(redo);
        }
        if let Some(view) = requested_channel_view.take() {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                tab.channel_view = view;
                tab.channel_view_user_set = true;
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
        // A focused topbar volume fader owns the arrows outright. Consumption
        // is not enough to keep them out of here: `key_down` below reads the
        // held-key set, which `consume_key` does not touch.
        if self.surface_keys_allowed(UiSurface::Editor)
            && tab_samples_len > 0
            && !self.topbar_volume_owns_arrows(ctx)
        {
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
                let repeat_delay = SEEK_REPEAT_DELAY;
                let repeat_fast = SEEK_REPEAT_FAST;
                let repeat_slow = SEEK_REPEAT_SLOW;
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
                            let interval = if elapsed >= SEEK_REPEAT_ACCELERATE_AFTER {
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
                    let max_display = tab_samples_len.saturating_sub(1);
                    let cur_display = playhead_display_now.min(max_display);
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
                    let mut single_sample_seek = false;
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
                        single_sample_seek = step_samples == 1;
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
                    let mut new_display = raw_target.min(max_display);
                    new_display = Self::stop_at_landmark_if_needed(
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
                            // At single-display-sample steps, converting the
                            // new display position back to an audio sample
                            // can round-trip to the same audio position when
                            // the display and audio sample rates differ
                            // (e.g. high zoom on a file whose native rate
                            // differs from the output rate), leaving the
                            // playhead stuck. Step the audio position by one
                            // sample directly in that case instead.
                            request_seek = Some(if single_sample_seek {
                                let cur_audio = map_display_to_audio(tab, cur_display);
                                if dir > 0 {
                                    cur_audio.saturating_add(1)
                                } else {
                                    cur_audio.saturating_sub(1)
                                }
                            } else {
                                map_display_to_audio(tab, new_display)
                            });
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
        // Transport time, read before the tab borrow below: the canvas's
        // context menu needs it and cannot ask `self` for anything once
        // `let tab = &mut self.tabs[..]` is live. `None` while nothing is
        // loaded, which is why the menu falls back to the drawn playhead.
        let transport_source_sec = self.playback_current_source_time_sec();
        let tab_path = self
            .tabs
            .get(tab_idx)
            .map(|tab| tab.path.clone())
            .unwrap_or_default();
        let tab_file_name = crate::app::ui::comments::file_label(&tab_path);
        // pending actions to perform after UI borrows end
        let mut pending_comment_ref: Option<(PathBuf, Option<crate::app::comments::CommentAnchor>)> =
            None;
         // keep-only (optional)
        let mut pending_spectral_warp_preview = false;
        let mut pending_pencil_preview_refresh = false;
        let mut pending_spectral_brush_preview = false;
        let mut pending_harmonic_click: Option<(usize, f32)> = None;
        let mut pending_scrub_begin: Option<u64> = None;
        let mut pending_scrub_update: Option<usize> = None;
        let mut pending_scrub_end = false;
         // (tab_idx, candidate_idx)
                                                                        // Loop/marker apply handled via commit flags below.
        let mut stop_playback = false;
        // Snapshot busy state and prepare deferred overlay job.
        // IMPORTANT: Do NOT call `self.*` (which takes &mut self) while holding `let tab = &mut self.tabs[...]`.
        // That pattern triggers borrow checker error E0499. Defer such calls to after the UI closures.
        let tab_path = self.tabs[tab_idx].path.clone();
        let gain_db = self
            .tabs
            .get(tab_idx)
            .map(|tab| self.pending_gain_db_for_path(&tab.path))
            .unwrap_or(0.0);
        let decode_status = self.editor_decode_ui_status(Some(tab_path.as_path()));
        let mut perf_mixdown_ms: Option<f32> = None;
        let mut perf_wave_render_ms: Option<f32> = None;
        let mut waveform_render_started: Option<std::time::Instant> = None;
        let mut waveform_scratch = wf_cache::WaveformScratch::default();
        let mut waveform_query_ms_total = 0.0f32;
        let mut waveform_draw_ms_total = 0.0f32;
        let pixels_per_point = ctx.pixels_per_point();
        // Split canvas and inspector. Narrow windows stack vertically so the
        // waveform never collapses to a zero-width strip.
        let stacked_editor = avail.x < 860.0;
        let min_canvas_w = 320.0f32;
        let min_inspector_w = 240.0f32;
        // User-resizable inspector width (drag the divider between canvas and
        // inspector); remembered for the session via egui memory.
        let inspector_width_id = egui::Id::new("editor_inspector_width");
        // The mini meter's height, set by the splitter above it. Remembered
        // for the session like the Inspector's width, and shared by every tab
        // so switching tabs does not move the meter.
        let mini_meter_height_id = egui::Id::new("editor_mini_meter_height");
        let stored_inspector_w: f32 =
            ctx.data_mut(|d| *d.get_temp_mut_or(inspector_width_id, 310.0f32));
        let max_inspector_w = (avail.x * 0.5).max(min_inspector_w);
        let split_spacing = ui.spacing().item_spacing.x;
        let divider_w = if stacked_editor { 0.0 } else { 6.0 };
        let split_avail_w = if stacked_editor {
            (avail.x - split_spacing).max(0.0)
        } else {
            (avail.x - split_spacing * 2.0 - divider_w).max(0.0)
        };
        let inspector_w = if stacked_editor {
            split_avail_w.max(1.0)
        } else {
            let available = (split_avail_w - min_canvas_w).max(min_inspector_w);
            stored_inspector_w
                .clamp(min_inspector_w, max_inspector_w)
                .min(available)
                .min(split_avail_w)
        };
        let canvas_w = if stacked_editor {
            split_avail_w.max(1.0)
        } else {
            (split_avail_w - inspector_w).max(min_canvas_w.min(split_avail_w))
        };
        let canvas_area_h = if stacked_editor {
            (avail.y * 0.45).clamp(180.0, (avail.y - 260.0).max(180.0))
        } else {
            avail.y
        };
        let inspector_area_h = if stacked_editor {
            (avail.y - canvas_area_h - split_spacing).max(180.0)
        } else {
            avail.y
        };
        // Resolved before `self` is borrowed by the body closure below.
        let editor_keys_allowed = self.surface_keys_allowed(UiSurface::Editor);
        // An object-audio tab playing through its object mix is metered as
        // its 7.1.4 bed: the tab holds no samples of its own, and the bed is
        // what is heard.
        let object_bed_meter = self
            .tabs
            .get(tab_idx)
            .filter(|tab| {
                self.audio.object_mix_active()
                    && matches!(
                        &self.playback_session.source,
                        crate::app::PlaybackSourceKind::EditorTab(path) if *path == tab.path
                    )
            })
            .and_then(|_| self.audio.object_bed_recent(crate::audio::OBJECT_BED_TAP_FRAMES));
        // The file's speakers, for the lane labels.
        let lane_speaker_layout = self
            .tabs
            .get(tab_idx)
            .and_then(|tab| self.channel_layout_for(&tab.path, tab.ch_samples.len()));
        let mini_meter_layout = if object_bed_meter.is_some() {
            crate::audio_channels::standard_layout_vec(crate::spatial::panner::BED_CHANNELS)
        } else {
            lane_speaker_layout.clone()
        };
        // The Panner's panel: the speakers and where each channel goes.
        // The file the headphone rendering is playing now, if any.
        let hrtf_active_path = match (&self.hrtf_runtime.status, &self.playback_session.source) {
            (
                crate::app::hrtf_ops::HrtfStatus::Active { .. },
                crate::app::PlaybackSourceKind::EditorTab(path)
                | crate::app::PlaybackSourceKind::ListPreview(path),
            ) => Some(path.clone()),
            _ => None,
        };
        {
            let mut draw_editor_body = |ui: &mut egui::Ui| {
                let tab = &mut self.tabs[tab_idx];
                // The draft as the canvas found it: a curve point, the pitch
                // line, a fade handle or a spectral stroke the canvas moves
                // this frame is a change to the tool's settings like any other.
                let canvas_draft_before = crate::app::tool_session::DraftSnapshot::capture(tab);
                // Canvas area
                let mut need_restore_preview = false;
                let mut pending_selection_stretch_apply: Option<(
                    f32,
                    (usize, usize),
                    EditorApplyViewportRestore,
                )> = None;
                if discard_preview_for_view_change {
                    need_restore_preview = true;
                }
                ui.allocate_ui_with_layout(
                    egui::vec2(canvas_w, canvas_area_h),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                    // Layout parameters. The lane split is settled before the
                    // canvas is allocated because its height depends on it.
                    let view_mode = tab.leaf_view_mode();
                    let (use_mixdown, visible_channels, lane_count) =
                        Self::editor_lane_layout(tab, view_mode);
                    // The column, top to bottom: the waveform, the time
                    // navigator, the splitter and the mini meter. The meter
                    // sits on the column's bottom edge at the height the
                    // splitter last left it and the waveform takes the rest,
                    // so neither moves when the window only gets wider. Lanes
                    // that do not fit the waveform scroll (`LaneGeometry`).
                    let column_rect = ui.max_rect();
                    let nav_reserved_h = ui.text_style_height(&TextStyle::Small)
                        + ui.spacing().item_spacing.y * 3.0
                        + lanes_layout::TIME_NAV_H;
                    let meter_pref_h: f32 = ctx.data_mut(|d| {
                        *d.get_temp_mut_or(
                            mini_meter_height_id,
                            lanes_layout::MINI_METER_DEFAULT_H,
                        )
                    });
                    let column_layout = lanes_layout::editor_column_layout(
                        column_rect.height(),
                        nav_reserved_h,
                        meter_pref_h,
                    );
                    let canvas_h = column_layout.wave_h;
                    let (resp, painter) = ui.allocate_painter(egui::vec2(canvas_w, canvas_h), Sense::click_and_drag());
                    let rect = resp.rect;
                    let w = rect.width().max(1.0); let h = rect.height().max(1.0);
                    let mut hover_cursor: Option<egui::CursorIcon> = None;
                    painter.rect_filled(rect, 0.0, Color32::from_rgb(16,16,18));
                    // Where the lanes are. Settled before the right rail is
                    // laid out, because a scrollbar takes its width from it.
                    let lane_geom = LaneGeometry::new(rect.top(), h, lane_count, tab.lane_scroll_px);
                    tab.lane_scroll_px = lane_geom.scroll_px;
                    tab.last_lane_geometry = Some(lane_geom);
                    let lane_scrollbar_rect = lane_geom.overflows().then(|| {
                        egui::Rect::from_min_max(
                            egui::pos2(rect.right() - lanes_layout::LANE_SCROLLBAR_W, rect.top()),
                            rect.max,
                        )
                    });
                    tab.last_lane_scrollbar_rect = lane_scrollbar_rect;
                    let lane_scrollbar_reserved_w = if lane_scrollbar_rect.is_some() {
                        lanes_layout::LANE_SCROLLBAR_W + lanes_layout::LANE_SCROLLBAR_GAP
                    } else {
                        0.0
                    };
                    // The right rail (amplitude navigator, colour range) lays
                    // out from here leftward.
                    let rail_right = rect.right() - lane_scrollbar_reserved_w;
                    let gutter_w = 44.0;
                    let show_amplitude_navigator = matches!(
                        view_mode,
                        ViewMode::Waveform | ViewMode::Spectrogram | ViewMode::Log | ViewMode::Mel
                    );
                    let show_spectro_color_range = matches!(
                        view_mode,
                        ViewMode::Spectrogram | ViewMode::Log | ViewMode::Mel
                    );
                    let amplitude_nav_gap = if show_amplitude_navigator { 6.0 } else { 0.0 };
                    let amplitude_nav_right_pad = if show_amplitude_navigator { 6.0 } else { 0.0 };
                    let amplitude_nav_strip_w = if show_amplitude_navigator { 18.0 } else { 0.0 };
                    let spectro_color_range_gap = if show_spectro_color_range { 6.0 } else { 0.0 };
                    let spectro_color_range_w = if show_spectro_color_range { 44.0 } else { 0.0 };
                    let amplitude_nav_reserved_w =
                        amplitude_nav_gap
                            + amplitude_nav_right_pad
                            + amplitude_nav_strip_w
                            + spectro_color_range_gap
                            + spectro_color_range_w
                            + lane_scrollbar_reserved_w;
                    let wave_left = rect.left() + gutter_w;
                    let wave_w = (w - gutter_w - amplitude_nav_reserved_w).max(1.0);
                    let amplitude_nav_rect = if show_amplitude_navigator {
                        Some(egui::Rect::from_min_max(
                            egui::pos2(rail_right - amplitude_nav_right_pad - amplitude_nav_strip_w, rect.top() + 10.0),
                            egui::pos2(rail_right - amplitude_nav_right_pad, rect.bottom() - 10.0),
                        ))
                    } else {
                        None
                    };
                    let spectro_color_range_rect = if show_spectro_color_range {
                        amplitude_nav_rect.map(|amp_rect| {
                            egui::Rect::from_min_max(
                                egui::pos2(
                                    amp_rect.left()
                                        - spectro_color_range_gap
                                        - spectro_color_range_w,
                                    rect.top() + 10.0,
                                ),
                                egui::pos2(
                                    amp_rect.left() - spectro_color_range_gap,
                                    rect.bottom() - 10.0,
                                ),
                            )
                        })
                    } else {
                        None
                    };
                    tab.last_amplitude_nav_rect = amplitude_nav_rect;
                    tab.last_wave_canvas_rect = Some(rect);
                    tab.last_amplitude_viewport_rect = amplitude_nav_rect.map(|nav_rect| {
                        Self::amplitude_nav_viewport_rect(
                            nav_rect,
                            tab.vertical_zoom,
                            tab.vertical_view_center,
                        )
                    });
                    let lane_h = lane_geom.lane_h;
                    // The lanes any part of which is on screen: the only ones
                    // drawn, however many channels the file has.
                    let visible_lanes = lane_geom.visible_lanes();

                    // Visual amplitude scale: assume Volume=0 dB for display; apply per-file Gain only
                    let scale = db_to_amp(gain_db);

                    // Initialize zoom to fit if unset (show whole file)
                    let display_samples_len = Self::editor_display_samples_len(tab);
                    if display_samples_len > 0 && tab.samples_per_px <= 0.0 {
                        let fit_spp = Self::editor_fit_samples_per_px(display_samples_len, wave_w);
                        tab.samples_per_px = fit_spp;
                        Self::editor_set_view_offset(tab, 0);
                        Self::invalidate_editor_viewport_cache(tab);
                        ctx.request_repaint();
                    }
                    // Keep the same center sample anchored when the window width changes.
                    if display_samples_len > 0 {
                        let spp = tab.samples_per_px.max(0.0001);
                        let old_wave_w = tab.last_wave_w;
                        if old_wave_w > 0.0 && (old_wave_w - wave_w).abs() > 0.5 {
                            if Self::editor_is_whole_file_fit(tab, display_samples_len, old_wave_w)
                            {
                                tab.samples_per_px =
                                    Self::editor_fit_samples_per_px(display_samples_len, wave_w);
                                Self::editor_set_view_offset(tab, 0);
                                Self::invalidate_editor_viewport_cache(tab);
                                ctx.request_repaint();
                            } else {
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
                                    Self::editor_set_view_offset_exact(
                                        tab,
                                        next_exact,
                                        new_geom.max_left(),
                                    );
                                    Self::invalidate_editor_viewport_cache(tab);
                                    ctx.request_repaint();
                                }
                            }
                        } else if old_wave_w <= 0.0 {
                            Self::invalidate_editor_viewport_cache(tab);
                            ctx.request_repaint();
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
            // The spectral views render the lanes on screen (and one either
            // side), never the whole stack: a texture one lane tall per
            // channel outgrows the GPU's limit long before the channel count
            // stops being plausible.
            let lane_window = lane_geom.window(lanes_layout::LANE_WINDOW_OVERSCAN);
            pending_viewport_hint = matches!(
                view_mode,
                ViewMode::Spectrogram
                    | ViewMode::Log
                    | ViewMode::Mel
                    | ViewMode::Tempogram
                    | ViewMode::Chromagram
                    | ViewMode::World
            )
            .then_some(crate::app::editor_viewport::EditorViewportHint {
                view_mode,
                display_samples_len,
                start,
                end,
                wave_width_px,
                lane_height_px,
                lane_count,
                first_lane: lane_window.start,
                window_lanes: lane_window.len(),
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
                    | ViewMode::World
            ) {
                    Some(EditorViewportRenderKey {
                        kind: EditorViewportPayloadKind::Spectral,
                        view_mode,
                        source_generation: tab.viewport_source_generation,
                        display_samples_len,
                        start,
                        end,
                        lane_count: lane_count.max(1),
                        first_lane: lane_window.start,
                        window_lanes: lane_window.len(),
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
                        let num = tab.time_sig_numerator.max(1) as f32; // beats per bar

                        // Sub-beat + beat steps, then bar-aligned multiples (1 bar, 2 bar, …)
                        let mut steps: Vec<f32> = vec![
                            1.0/64.0, 1.0/32.0, 1.0/16.0, 1.0/8.0, 1.0/4.0, 0.5, 1.0,
                        ];
                        let mut bar_mult = 1.0f32;
                        for _ in 0..12 {
                            steps.push(num * bar_mult);
                            bar_mult *= 2.0;
                            if num * bar_mult > 65536.0 { break; }
                        }
                        steps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                        steps.dedup_by(|a, b| (*a - *b).abs() < 0.001);

                        let mut step_beats = *steps.last().unwrap();
                        for &s in &steps {
                            if px_per_beat * s >= min_px {
                                step_beats = s;
                                break;
                            }
                        }

                        let b0 = (t0 - offset_sec) / beat_sec;
                        let b1 = (t1 - offset_sec) / beat_sec;
                        let start_beat = (b0 / step_beats).floor() * step_beats;
                        let mut beat = start_beat;

                        // Bar lines are noticeably brighter than beat/sub-beat lines
                        let bar_line_col = Color32::from_rgb(72, 72, 88);
                        let beat_line_col = grid_col;

                        while beat <= b1 + step_beats * 0.5 {
                            let t = offset_sec + beat * beat_sec;
                            if t < t0 || t > t1 {
                                beat += step_beats;
                                continue;
                            }
                            let s_idx = (t * sr).round() as isize;
                            let x = geom.sample_boundary_x(s_idx.max(0) as usize);

                            // Bar boundary check with small floating-point tolerance
                            let beat_in_bar = beat.rem_euclid(num);
                            let is_bar = beat_in_bar < 0.001 || beat_in_bar > num - 0.001;

                            painter.line_segment(
                                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                                egui::Stroke::new(
                                    if is_bar { 1.5_f32 } else { 1.0_f32 },
                                    if is_bar { bar_line_col } else { beat_line_col },
                                ),
                            );

                            if px_per_beat * step_beats >= 70.0 {
                                let label = if is_bar {
                                    // Bar number, 1-indexed from beat 0
                                    let bar = (beat / num).round() as i64 + 1;
                                    format!("|{bar}")
                                } else if step_beats >= 1.0 {
                                    // Whole beat within bar (2, 3, 4 …)
                                    format!(".{}", beat_in_bar.round() as i32 + 1)
                                } else {
                                    // Sub-beat: show fractional beat count
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
                            painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(1.0_f32, grid_col));
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
                        if spec_cache.is_some() {
                            touch_spectro_cache = true;
                            let current_viewport_cache = current_feature_viewport_key
                                .as_ref()
                                .and_then(|key| Self::best_editor_viewport_cache(tab, key));
                            if let Some(crate::app::types::EditorViewportRenderCache {
                                key: texture_key,
                                payload:
                                    crate::app::types::EditorViewportCachePayload::Image {
                                        texture: Some(texture),
                                        ..
                                    },
                                ..
                            }) = current_viewport_cache
                            {
                                // At the lanes this render covers, which after
                                // a scroll are not yet the lanes on screen.
                                painter.image(
                                    texture.id(),
                                    egui::Rect::from_min_size(
                                        egui::pos2(wave_left, lane_geom.lane_top(texture_key.first_lane)),
                                        egui::vec2(wave_w, lane_h * texture_key.window_lanes.max(1) as f32),
                                    ),
                                    egui::Rect::from_min_max(
                                        egui::pos2(0.0, 0.0),
                                        egui::pos2(1.0, 1.0),
                                    ),
                                    Color32::WHITE,
                                );
                            } else {
                                // Rendering and texture creation stay on the viewport worker.
                                // Keeping the last compatible texture avoids a blank flash while
                                // zooming, without doing a full spectral render on the UI thread.
                                let fid = TextStyle::Monospace.resolve(ui.style());
                                painter.text(
                                    egui::pos2(wave_left + 6.0, rect.top() + 6.0),
                                    egui::Align2::LEFT_TOP,
                                    if spec_loading {
                                        "Building spectrogram..."
                                    } else {
                                        "Rendering spectrogram..."
                                    },
                                    fid,
                                    Color32::GRAY,
                                );
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
                                // Before the spectrogram exists: the rate it
                                // will be computed at, the tab's buffer rate.
                                .unwrap_or(tab.buffer_sample_rate);
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
                            let tick_stroke = egui::Stroke::new(1.0_f32, tick_col);
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
                            for ci in visible_lanes.clone() {
                                let lane_top = lane_geom.lane_top(ci);
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
                            .and_then(|key| Self::best_editor_viewport_cache(tab, key));
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
                        } else {
                            let fid = TextStyle::Monospace.resolve(ui.style());
                            let msg = if feature_loading {
                                "Building tempogram..."
                            } else if tempogram_data.is_some() {
                                "Rendering tempogram..."
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
                        {
                            let fid = TextStyle::Monospace.resolve(ui.style());
                            let tick_col = Color32::from_rgb(140, 150, 165);
                            let tick_stroke = egui::Stroke::new(1.0_f32, tick_col);
                            let (visible_min, visible_max) = Self::editor_vertical_range_for_view(
                                view_mode,
                                tab.vertical_zoom,
                                tab.vertical_view_center,
                                &self.spectro_cfg,
                            );
                            let visible_frac_of = |frac: f32| {
                                if (visible_max - visible_min).abs() < f32::EPSILON {
                                    0.0
                                } else {
                                    ((frac - visible_min) / (visible_max - visible_min))
                                        .clamp(0.0, 1.0)
                                }
                            };
                            for bpm in [30.0, 60.0, 90.0, 120.0, 150.0, 180.0, 240.0, 300.0] {
                                let frac = tempogram_data
                                    .as_ref()
                                    .and_then(|data| Self::tempogram_axis_position(data, bpm))
                                    .unwrap_or(((bpm - 30.0) / 270.0).clamp(0.0, 1.0));
                                let visible_frac = visible_frac_of(frac);
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
                            // Estimated BPM guide: horizontal accent line across
                            // the lane so the ridge can be read directly.
                            if let Some(data) = tempogram_data.as_ref() {
                                if let Some(bpm) = data.estimated_bpm {
                                    if let Some(frac) = Self::tempogram_axis_position(data, bpm) {
                                        let y = lane_rect.bottom()
                                            - visible_frac_of(frac) * lane_rect.height();
                                        let accent = Color32::from_rgb(120, 220, 160);
                                        painter.line_segment(
                                            [
                                                egui::pos2(lane_rect.left(), y),
                                                egui::pos2(lane_rect.right(), y),
                                            ],
                                            egui::Stroke::new(1.0_f32, accent),
                                        );
                                        painter.text(
                                            egui::pos2(lane_rect.right() - 4.0, y - 2.0),
                                            egui::Align2::RIGHT_BOTTOM,
                                            format!("\u{2248} {bpm:.1} BPM"),
                                            fid.clone(),
                                            accent,
                                        );
                                    }
                                }
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
                            .and_then(|key| Self::best_editor_viewport_cache(tab, key));
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
                        } else {
                            let fid = TextStyle::Monospace.resolve(ui.style());
                            let msg = if feature_loading {
                                "Building chromagram..."
                            } else if chromagram_data.is_some() {
                                "Rendering chromagram..."
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
                        {
                            let fid = TextStyle::Monospace.resolve(ui.style());
                            let tick_col = Color32::from_rgb(140, 150, 165);
                            let tick_stroke = egui::Stroke::new(1.0_f32, tick_col);
                            let (visible_min, visible_max) = Self::editor_vertical_range_for_view(
                                view_mode,
                                tab.vertical_zoom,
                                tab.vertical_view_center,
                                &self.spectro_cfg,
                            );
                            let estimated_key = chromagram_data
                                .as_ref()
                                .and_then(|data| data.estimated_key.as_deref());
                            for bin in 0..12 {
                                // Labels sit at each band's center, matching the
                                // equal-height bands the image renderer paints.
                                let frac = (bin as f32 + 0.5) / 12.0;
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
                                let label = Self::chroma_label_for_bin(bin);
                                let is_key = estimated_key == Some(label);
                                painter.text(
                                    egui::pos2(rect.left() + 2.0, y),
                                    egui::Align2::LEFT_CENTER,
                                    label,
                                    fid.clone(),
                                    if is_key {
                                        Color32::from_rgb(120, 220, 160)
                                    } else {
                                        tick_col
                                    },
                                );
                            }
                        }
                    }
                    ViewMode::World => {
                        let lane_rect = egui::Rect::from_min_size(
                            egui::pos2(wave_left, rect.top()),
                            egui::vec2(wave_w, h),
                        );
                        let current_viewport_cache = current_feature_viewport_key
                            .as_ref()
                            .and_then(|key| Self::best_editor_viewport_cache(tab, key));
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
                        } else {
                            let fid = TextStyle::Monospace.resolve(ui.style());
                            let msg = if feature_loading {
                                let pct = feature_progress
                                    .map(|(done, total, _)| {
                                        if total > 0 {
                                            (done as f32 / total as f32 * 100.0).round()
                                        } else {
                                            0.0
                                        }
                                    })
                                    .unwrap_or(0.0);
                                format!(
                                    "Analyzing WORLD features (F0 / spectral envelope)... {pct:.0}%"
                                )
                            } else if world_data.is_some() {
                                "Rendering WORLD features...".to_string()
                            } else {
                                "WORLD features not ready".to_string()
                            };
                            painter.text(
                                egui::pos2(wave_left + 6.0, rect.top() + 6.0),
                                egui::Align2::LEFT_TOP,
                                msg,
                                fid,
                                Color32::GRAY,
                            );
                        }
                        if let Some(data) = world_data {
                            let (visible_min, visible_max) = Self::editor_vertical_range_for_view(
                                view_mode,
                                tab.vertical_zoom,
                                tab.vertical_view_center,
                                &self.spectro_cfg,
                            );
                            let span = (visible_max - visible_min).max(f32::EPSILON);
                            let f0_focus = tab.world_f0_focus;
                            let y_for_freq = |freq: f32| {
                                let frac =
                                    Self::world_view_freq_to_frac(freq, data.sample_rate, f0_focus);
                                let visible_frac = ((frac - visible_min) / span).clamp(0.0, 1.0);
                                lane_rect.bottom() - visible_frac * lane_rect.height()
                            };
                            // Frequency axis ticks along the left gutter.
                            let fid = TextStyle::Monospace.resolve(ui.style());
                            let tick_col = Color32::from_rgb(140, 150, 165);
                            let tick_stroke = egui::Stroke::new(1.0_f32, tick_col);
                            let nyquist = data.sample_rate.max(1) as f32 * 0.5;
                            for (freq, label) in [
                                (100.0f32, "100"),
                                (200.0, "200"),
                                (500.0, "500"),
                                (1_000.0, "1k"),
                                (2_000.0, "2k"),
                                (5_000.0, "5k"),
                                (10_000.0, "10k"),
                            ] {
                                if freq > nyquist {
                                    continue;
                                }
                                let (axis_lo, axis_hi) =
                                    Self::world_view_freq_bounds(data.sample_rate, f0_focus);
                                if freq < axis_lo * 1.01 || freq > axis_hi * 0.999 {
                                    continue;
                                }
                                let frac =
                                    Self::world_view_freq_to_frac(freq, data.sample_rate, f0_focus);
                                let visible_frac = (frac - visible_min) / span;
                                if !(0.0..=1.0).contains(&visible_frac) {
                                    continue;
                                }
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
                                    label,
                                    fid.clone(),
                                    tick_col,
                                );
                            }
                            // F0 trajectories: analyzed curve, plus the edit
                            // draft on top when it exists (broken at unvoiced
                            // frames in both cases).
                            let frame_step = data.frame_step.max(1);
                            let visible_len = end.saturating_sub(start).max(1) as f32;
                            let analyzed_color = Color32::from_rgb(80, 230, 255);
                            let draft_color = Color32::from_rgb(255, 176, 64);
                            let draft_values: Option<&[f32]> = tab
                                .world_f0_draft
                                .as_ref()
                                .filter(|draft| {
                                    draft.source_frames == data.frames
                                        && (draft.dirty || draft.edit_enabled)
                                })
                                .map(|draft| draft.values.as_slice());
                            let first_frame = start / frame_step;
                            let last_frame = ((end / frame_step) + 2).min(data.frames);
                            // Each curve gets a dark halo pass underneath so
                            // the line stays readable on bright heatmap areas.
                            // Decimate to at most ~2 points per pixel so
                            // long clips stay cheap to draw; a window with any
                            // unvoiced frame breaks the line like the raw data
                            // would.
                            let frame_span = last_frame.saturating_sub(first_frame).max(1);
                            let max_points = (wave_w.max(32.0) as usize) * 2;
                            let frame_step_draw = (frame_span / max_points.max(1)).max(1);
                            let draw_curve = |values: &[f32], color: Color32, width: f32| {
                                let halo = Color32::from_rgba_unmultiplied(8, 10, 14, 210);
                                for (pass_color, pass_width) in
                                    [(halo, width + 2.6), (color, width)]
                                {
                                    let stroke = egui::Stroke::new(pass_width, pass_color);
                                    let mut last_pt: Option<egui::Pos2> = None;
                                    let mut frame = first_frame;
                                    while frame < last_frame {
                                        let window_end =
                                            (frame + frame_step_draw).min(last_frame);
                                        let mut f0 = 0.0f32;
                                        let mut voiced = true;
                                        for j in frame..window_end {
                                            let v = values.get(j).copied().unwrap_or(0.0);
                                            if v <= 0.0 {
                                                voiced = false;
                                                break;
                                            }
                                            if j == frame {
                                                f0 = v;
                                            }
                                        }
                                        if !voiced {
                                            last_pt = None;
                                            frame = window_end;
                                            continue;
                                        }
                                        let sample = frame * frame_step;
                                        let x = wave_left
                                            + ((sample.saturating_sub(start)) as f32
                                                / visible_len)
                                                * wave_w;
                                        let pt = egui::pos2(
                                            x.clamp(wave_left, wave_left + wave_w),
                                            y_for_freq(f0),
                                        );
                                        if let Some(prev) = last_pt {
                                            painter.line_segment([prev, pt], stroke);
                                        } else {
                                            painter.circle_filled(
                                                pt,
                                                pass_width * 0.9,
                                                pass_color,
                                            );
                                        }
                                        last_pt = Some(pt);
                                        frame = window_end;
                                    }
                                }
                            };
                            if let Some(draft) = draft_values {
                                draw_curve(
                                    &data.f0_values,
                                    Color32::from_rgba_unmultiplied(80, 230, 255, 110),
                                    1.4,
                                );
                                draw_curve(draft, draft_color, 2.4);
                            } else {
                                draw_curve(&data.f0_values, analyzed_color, 2.4);
                            }
                            // Live F0 readout at the playhead (edited value
                            // when a draft is active).
                            let cur_frame = playhead_display_now / frame_step;
                            let cur_values = draft_values.unwrap_or(&data.f0_values);
                            let cur_f0 = cur_values
                                .get(cur_frame.min(data.frames.saturating_sub(1)))
                                .copied()
                                .unwrap_or(0.0);
                            let readout = if cur_f0 > 0.0 {
                                format!("F0 {cur_f0:.1} Hz")
                            } else {
                                "F0 --".to_string()
                            };
                            let readout_color = if draft_values.is_some() {
                                draft_color
                            } else {
                                analyzed_color
                            };
                            painter.text(
                                egui::pos2(lane_rect.right() - 8.0, lane_rect.top() + 6.0),
                                egui::Align2::RIGHT_TOP,
                                readout,
                                fid,
                                readout_color,
                            );
                            if tab
                                .world_f0_draft
                                .as_ref()
                                .map(|draft| draft.edit_enabled)
                                .unwrap_or(false)
                            {
                                painter.text(
                                    egui::pos2(lane_rect.left() + 6.0, lane_rect.top() + 6.0),
                                    egui::Align2::LEFT_TOP,
                                    "F0 EDIT: left-drag draw / right-drag erase",
                                    egui::FontId::monospace(10.0),
                                    draft_color,
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
            if view_mode == ViewMode::Waveform && display_samples_len > 0 && editor_keys_allowed {
                let zoom_in = ctx.input(|i| i.key_pressed(egui::Key::ArrowUp));
                let zoom_out = ctx.input(|i| i.key_pressed(egui::Key::ArrowDown));
                if zoom_in || zoom_out {
                    let factor = if zoom_in { 0.9 } else { 1.1 };
                    Self::editor_apply_zoom_factor(
                        tab,
                        factor,
                        self.horizontal_zoom_anchor_mode,
                        display_samples_len,
                        wave_left,
                        wave_w,
                        None,
                        playhead_display_now,
                    );
                }
            }

            // Detect hover using pointer position against our canvas rect (robust across senses)
            let pointer_pos = ui.input(|i| i.pointer.hover_pos());
            let pointer_over_waveform = editor_scroll_active
                && ui.rect_contains_pointer(rect)
                && pointer_pos.is_some_and(|p| {
                rect.contains(p)
                    && amplitude_nav_rect
                        .map(|amp_rect| !amp_rect.contains(p))
                        .unwrap_or(true)
                    && spectro_color_range_rect
                        .map(|range_rect| !range_rect.contains(p))
                        .unwrap_or(true)
            });
            // Lanes that overflow the canvas scroll vertically: Alt+wheel
            // anywhere over it (egui's own vertical-scroll modifier, which
            // also folds a sideways wheel into a vertical one), or the plain
            // wheel over the gutter and the scrollbar, where there is no time
            // position to zoom around. Shift (pan) and Ctrl (zoom) keep their
            // meaning everywhere: egui turns both into something other than a
            // vertical delta before this reads it.
            let lane_wheel_dy = if pointer_over_waveform && lane_geom.overflows() {
                let (wheel_y, alt) =
                    ui.input(|i| (i.smooth_scroll_delta.y, i.modifiers.alt));
                let over_lane_strip = pointer_pos.is_some_and(|p| {
                    p.x < wave_left || lane_scrollbar_rect.is_some_and(|bar| bar.contains(p))
                });
                (wheel_y != 0.0 && (alt || over_lane_strip)).then_some(-wheel_y)
            } else {
                None
            };
            if let Some(dy) = lane_wheel_dy {
                tab.lane_scroll_px = lane_geom.scrolled_by(dy);
                ctx.request_repaint();
            }
            if pointer_over_waveform && lane_wheel_dy.is_none() {
                // Zoom with wheel/pinch over this widget.
                // `zoom_delta` captures ctrl/cmd+wheel and pinch gestures robustly.
                let (wheel_smooth, zoom_delta, modifiers) = ui.input(|i| {
                    (
                        i.smooth_scroll_delta,
                        i.zoom_delta() as f32,
                        i.modifiers,
                    )
                });
                let wheel = wheel_smooth;
                let scroll_y = wheel.y;
                let pointer_x = resp.hover_pos().map(|p| p.x).filter(|_| pointer_over_waveform);
                let wheel_has_scroll = wheel.x.abs() > 0.0 || scroll_y.abs() > 0.0;
                let horizontal_wheel_pan = !modifiers.shift
                    && wheel.x.abs() > 0.0
                    && wheel.x.abs() >= scroll_y.abs();
                let zoom_factor_from_input = if !wheel_has_scroll
                    && zoom_delta.is_finite()
                    && (zoom_delta - 1.0).abs() > 0.10
                {
                    // egui zoom_delta > 1 means "zoom in". For samples-per-pixel we invert it
                    // (and flip again when the user prefers reversed wheel-zoom direction).
                    let raw = if self.invert_wave_zoom_wheel {
                        zoom_delta.max(1e-3)
                    } else {
                        1.0 / zoom_delta.max(1e-3)
                    };
                    Some(raw.clamp(0.2, 5.0))
                } else {
                    None
                };
                // Zoom: plain wheel (unless Shift is held for pan, or the user
                // prefers wheel-scrolls mode) or gesture zoom (Ctrl+wheel/pinch).
                if (((scroll_y.abs() > 0.0)
                    && !modifiers.shift
                    && !horizontal_wheel_pan
                    && !self.editor_wheel_scrolls)
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
                    Self::editor_apply_zoom_factor(
                        tab,
                        factor,
                        self.horizontal_zoom_anchor_mode,
                        display_samples_len,
                        wave_left,
                        wave_w,
                        pointer_x,
                        playhead_display_now,
                    );
                }
                // Wheel-scrolls mode: a plain vertical wheel pans the view
                // horizontally (wheel up = view left, matching Shift+wheel).
                if self.editor_wheel_scrolls
                    && scroll_y.abs() > 0.0
                    && !modifiers.shift
                    && !horizontal_wheel_pan
                    && zoom_factor_from_input.is_none()
                    && display_samples_len > 0
                {
                    let vis = (wave_w * tab.samples_per_px).ceil() as usize;
                    let max_left = display_samples_len.saturating_sub(vis);
                    // Documented as equivalent to Shift+wheel, so it honors
                    // the same direction-inversion pref.
                    let pan_y = if self.invert_shift_wheel_pan {
                        -scroll_y
                    } else {
                        scroll_y
                    };
                    let next_exact =
                        tab.view_offset_exact + (-pan_y * tab.samples_per_px) as f64;
                    Self::editor_set_view_offset_exact(tab, next_exact, max_left);
                    ctx.request_repaint();
                }
                // Pan with trackpad horizontal wheel, or Shift + wheel for mouse wheels.
                if horizontal_wheel_pan && display_samples_len > 0 {
                    let vis = (wave_w * tab.samples_per_px).ceil() as usize;
                    let max_left = display_samples_len.saturating_sub(vis);
                    let next_exact =
                        tab.view_offset_exact + (wheel.x * tab.samples_per_px) as f64;
                    Self::editor_set_view_offset_exact(tab, next_exact, max_left);
                    ctx.request_repaint();
                }
                let mut scroll_for_pan = if wheel.x.abs() > 0.0 { wheel.x } else { wheel.y };
                if self.invert_shift_wheel_pan {
                    scroll_for_pan = -scroll_for_pan;
                }
                if modifiers.shift && scroll_for_pan.abs() > 0.0 && display_samples_len > 0 {
                    // Proportional, and measured against the viewport rather
                    // than a fixed 60 px.
                    //
                    // A fixed step meant crossing a long file took as many
                    // notches at any zoom, which is what made scrubbing through
                    // a long take feel like it went nowhere. Taking `.signum()`
                    // also threw away how far the wheel actually turned, and
                    // because egui smooths one physical notch across several
                    // frames, that turned one notch into as many steps as the
                    // frame rate happened to allow. Scaling the real delta
                    // fixes both: a notch moves the same fraction of what is on
                    // screen every time.
                    let delta_px =
                        -scroll_for_pan * Self::editor_horizontal_pan_gain(wave_w, self.editor_horizontal_scroll_speed);
                    let vis = (wave_w * tab.samples_per_px).ceil() as usize;
                    let max_left = display_samples_len.saturating_sub(vis);
                    let next_exact = tab.view_offset_exact + (delta_px * tab.samples_per_px) as f64;
                    Self::editor_set_view_offset_exact(tab, next_exact, max_left);
                    ctx.request_repaint();
                }
                // Pan with Middle drag.
                let mid_down = ui.input(|i| i.pointer.button_down(egui::PointerButton::Middle));
                if mid_down && display_samples_len > 0 {
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
            // ---- Scrub (Alt + primary drag): loop a ±40 ms window under
            // the pointer; release restores the previous loop/transport.
            {
                let alt_down = ui.input(|i| i.modifiers.alt);
                let scrubbing_here = self
                    .scrub_state
                    .as_ref()
                    .map(|s| s.tab_id == tab.tab_id)
                    .unwrap_or(false);
                if pointer_over_waveform
                    && alt_down
                    && !scrubbing_here
                    && display_samples_len > 0
                    && resp.drag_started_by(egui::PointerButton::Primary)
                {
                    pending_scrub_begin = Some(tab.tab_id);
                }
                if scrubbing_here || pending_scrub_begin.is_some() {
                    suppress_seek = true;
                    if let Some(pos) = resp
                        .interact_pointer_pos()
                        .or_else(|| ui.input(|i| i.pointer.hover_pos()))
                    {
                        let samp = geom
                            .x_to_display_sample(pos.x.clamp(wave_left, wave_left + wave_w));
                        pending_scrub_update = Some(map_display_to_audio(tab, samp));
                    }
                    let released = ui
                        .input(|i| i.pointer.button_released(egui::PointerButton::Primary))
                        || !ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary));
                    if scrubbing_here && released {
                        pending_scrub_end = true;
                        pending_scrub_update = None;
                    }
                    ctx.request_repaint();
                }
            }
            // WORLD F0 pencil editing: while enabled it owns the pointer on
            // this canvas (left-drag draws pitch, right-drag erases), so the
            // seek / marker / range-select handlers below are bypassed.
            let world_f0_editing = view_mode == ViewMode::World
                && world_data.is_some()
                && tab
                    .world_f0_draft
                    .as_ref()
                    .map(|draft| draft.edit_enabled)
                    .unwrap_or(false);
            if world_f0_editing {
                suppress_seek = true;
                if let Some(data) = world_data {
                    if resp.hovered() {
                        hover_cursor = Some(egui::CursorIcon::Crosshair);
                    }
                    let draw_btn = resp.drag_started_by(egui::PointerButton::Primary)
                        || resp.dragged_by(egui::PointerButton::Primary)
                        || resp.clicked_by(egui::PointerButton::Primary);
                    let erase_btn = resp.drag_started_by(egui::PointerButton::Secondary)
                        || resp.dragged_by(egui::PointerButton::Secondary)
                        || resp.clicked_by(egui::PointerButton::Secondary);
                    if draw_btn || erase_btn {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            let sample = geom.x_to_display_sample(
                                pos.x.clamp(wave_left, wave_left + wave_w),
                            );
                            let frame = sample / data.frame_step.max(1);
                            let (visible_min, visible_max) =
                                Self::editor_vertical_range_for_view(
                                    view_mode,
                                    tab.vertical_zoom,
                                    tab.vertical_view_center,
                                    &self.spectro_cfg,
                                );
                            let frac_local =
                                1.0 - ((pos.y - rect.top()) / h.max(1.0)).clamp(0.0, 1.0);
                            let frac = visible_min + frac_local * (visible_max - visible_min);
                            let freq = if erase_btn {
                                0.0
                            } else {
                                Self::world_view_frac_to_freq(
                                    frac,
                                    data.sample_rate,
                                    tab.world_f0_focus,
                                )
                            };
                            let draft = Self::world_f0_draft_mut(tab, data);
                            Self::world_f0_paint(draft, frame, freq);
                        }
                    }
                    if resp.drag_stopped_by(egui::PointerButton::Primary)
                        || resp.drag_stopped_by(egui::PointerButton::Secondary)
                    {
                        if let Some(draft) = tab.world_f0_draft.as_mut() {
                            draft.last_drag_frame = None;
                        }
                    }
                }
            }
            // ---- Tool canvas gestures (Waveform view) ----
            // The primary selection's edge handles are always available in
            // Waveform mode. A press on either one is armed before the active
            // tool sees the pointer; explicit Loop/Fade/Marker and envelope
            // handles retain priority when they overlap it.
            let wave_view_gestures = view_mode == ViewMode::Waveform && display_samples_len > 0;
            let gain_env_editing = wave_view_gestures
                && matches!(tab.active_tool, ToolKind::Gain)
                && tab.gain_env_enabled;
            let pitch_env_editing = wave_view_gestures
                && matches!(tab.active_tool, ToolKind::PitchShift)
                && tab.pitch_env_enabled;
            // Spectral warp editing owns the pointer on the Spec/Log views
            // (the input handling itself lives below, after the spectral
            // y<->Hz helpers are defined).
            let spectral_warp_editing = matches!(view_mode, ViewMode::Spectrogram | ViewMode::Log)
                && tab.spectral_warp_edit
                && display_samples_len > 0;
            // Spectral brush painting owns the pointer the same way (the
            // inspector keeps warp and brush editing mutually exclusive).
            let spectral_brush_editing = matches!(view_mode, ViewMode::Spectrogram | ViewMode::Log)
                && tab.spectral_brush_edit
                && !spectral_warp_editing
                && display_samples_len > 0;
            let canvas_h = rect.height().max(1.0);
            const GAIN_ENV_DB_RANGE: f32 = 24.0;
            const PITCH_SEMI_RANGE: f32 = 12.0;
            let frac_to_y = |frac: f32| rect.top() + (1.0 - frac.clamp(0.0, 1.0)) * canvas_h;
            let y_to_frac = |y: f32| (1.0 - (y - rect.top()) / canvas_h).clamp(0.0, 1.0);
            // Every breakpoint curve on the canvas shares its x axis.
            let curve_sample_to_x = |s: usize| geom.sample_center_x(s.min(display_samples_len));
            let curve_x_to_sample =
                |x: f32| geom.x_to_display_sample(x.clamp(wave_left, wave_left + wave_w));
            // The automation lane the canvas edits: the active tool's, picked
            // in the Inspector, switched on.
            let automation_param: Option<AutoParam> = tab
                .automation_selected
                .filter(|p| p.tool() == tab.active_tool)
                .filter(|p| tool_automation::lane(tab, *p).is_some_and(|l| l.enabled));

            let cancel_selection_stretch = !wave_view_gestures
                || ui.input(|input| {
                    input.key_pressed(egui::Key::Escape) || !input.raw.focused
                });
            if cancel_selection_stretch {
                if tab.selection_stretch_gesture.take().is_some()
                    && ui.input(|input| input.pointer.primary_down())
                {
                    tab.selection_stretch_cancel_until_release = true;
                }
            }
            if tab.selection_stretch_gesture.is_some_and(|gesture| {
                Self::editor_selected_range(tab) != Some(gesture.source_range)
            }) {
                tab.selection_stretch_gesture = None;
            }
            let cancelled_selection_stretch_claims_pointer =
                tab.selection_stretch_cancel_until_release;
            if cancelled_selection_stretch_claims_pointer
                && ui.input(|input| {
                    input.pointer.primary_released() || !input.pointer.primary_down()
                })
            {
                tab.selection_stretch_cancel_until_release = false;
            }
            if wave_view_gestures
                && pointer_over_waveform
                && tab.selection_stretch_gesture.is_none()
                && !cancelled_selection_stretch_claims_pointer
                && ui.input(|input| input.pointer.primary_pressed())
            {
                if let (Some((sel_s, sel_e)), Some(pos)) = (
                    Self::editor_selected_range(tab),
                    ui.input(|input| input.pointer.press_origin())
                        .or_else(|| ui.input(|input| input.pointer.hover_pos())),
                ) {
                    let near_x = |sample: usize, radius: f32| {
                        geom.contains_boundary(sample)
                            && (pos.x - geom.sample_boundary_x_unclamped(sample)).abs() <= radius
                    };
                    let press_in_stretch_band =
                        in_selection_stretch_band(rect.top(), pos.y);
                    let mut specialized_handle_hit = false;
                    match tab.active_tool {
                        // A loop edge almost always sits exactly on the
                        // selection edge (Set loop from selection), so the two
                        // grips overlap. Split them by height rather than
                        // letting one swallow the other: the tab at the top
                        // stretches, the line below it moves the loop marker.
                        ToolKind::LoopEdit if !press_in_stretch_band => {
                            if let Some((a, b)) = tab.loop_region {
                                specialized_handle_hit =
                                    near_x(a.min(b), LOOP_EDGE_GRAB_RADIUS)
                                        || near_x(a.max(b), LOOP_EDGE_GRAB_RADIUS);
                            }
                        }
                        ToolKind::Fade => {
                            let sr = tab.buffer_sample_rate.max(1) as f32;
                            let fade_in =
                                ((tab.tool_state.fade_in_ms.max(0.0) / 1000.0) * sr).round()
                                    as usize;
                            let fade_out =
                                ((tab.tool_state.fade_out_ms.max(0.0) / 1000.0) * sr).round()
                                    as usize;
                            specialized_handle_hit = (fade_in > 0
                                && near_x(fade_in.min(display_samples_len), 10.0))
                                || (fade_out > 0
                                    && near_x(
                                        display_samples_len.saturating_sub(fade_out),
                                        10.0,
                                    ));
                        }
                        ToolKind::Markers => {
                            specialized_handle_hit =
                                tab.markers.iter().any(|marker| near_x(marker.sample, 7.0));
                        }
                        ToolKind::Gain if tab.gain_env_enabled => {
                            specialized_handle_hit = tab.gain_env_points.iter().any(|point| {
                                let x = geom.sample_center_x(point.0.min(display_samples_len));
                                let y = frac_to_y(0.5 + point.1 / (GAIN_ENV_DB_RANGE * 2.0));
                                egui::pos2(x, y).distance(pos) <= 8.0
                            });
                        }
                        ToolKind::PitchShift if tab.pitch_env_enabled => {
                            specialized_handle_hit = tab.pitch_env_points.iter().any(|point| {
                                let x = geom.sample_center_x(point.0.min(display_samples_len));
                                let y = frac_to_y(0.5 + point.1 / (PITCH_SEMI_RANGE * 2.0));
                                egui::pos2(x, y).distance(pos) <= 8.0
                            });
                        }
                        ToolKind::Eq | ToolKind::Compressor | ToolKind::Limiter
                            if automation_param.is_some() =>
                        {
                            let st = tab.tool_state;
                            if let Some(lane) =
                                automation_param.and_then(|p| tool_automation::lane(tab, p))
                            {
                                specialized_handle_hit = lane.points.iter().any(|point| {
                                    let x = geom.sample_center_x(point.0.min(display_samples_len));
                                    let y = frac_to_y(lane.param.to_frac(&st, point.1));
                                    egui::pos2(x, y).distance(pos) <= curve_canvas::POINT_HIT_RADIUS
                                });
                            }
                        }
                        ToolKind::PitchShift => {
                            let semitones = tab.tool_state.pitch_semitones.clamp(
                                -PITCH_SEMI_RANGE,
                                PITCH_SEMI_RANGE,
                            );
                            let line_y = frac_to_y(
                                0.5 + semitones / (PITCH_SEMI_RANGE * 2.0),
                            );
                            specialized_handle_hit = (pos.y - line_y).abs() <= 8.0;
                        }
                        _ => {}
                    }

                    if !specialized_handle_hit && press_in_stretch_band && sel_e > sel_s {
                        let mut candidates = Vec::with_capacity(2);
                        if geom.contains_boundary(sel_s) {
                            candidates.push((geom.sample_boundary_x_unclamped(sel_s), 0usize));
                        }
                        if geom.contains_boundary(sel_e) {
                            candidates.push((geom.sample_boundary_x_unclamped(sel_e), 1usize));
                        }
                        if let Some(which) = nearest_handle(
                            &candidates,
                            pos.x,
                            SELECTION_EDGE_GRAB_RADIUS,
                        ) {
                            let edge = if which == 0 {
                                SelectionStretchEdge::Start
                            } else {
                                SelectionStretchEdge::End
                            };
                            let fixed_sample = match edge {
                                SelectionStretchEdge::Start => sel_e,
                                SelectionStretchEdge::End => sel_s,
                            };
                            tab.selection_stretch_gesture = Some(SelectionStretchGesture {
                                edge,
                                source_range: (sel_s, sel_e),
                                target_len: sel_e - sel_s,
                                press_pointer_x: pos.x,
                                fixed_screen_offset_px: geom
                                    .sample_boundary_x_unclamped(fixed_sample)
                                    - wave_left,
                                moved: false,
                            });
                        }
                    }
                }
            }
            // A flag any gesture sets while it owns the primary button; the
            // seek / range-select handlers below bail out when set.
            let mut tool_gesture_active = spectral_warp_editing
                || spectral_brush_editing
                || tab.selection_stretch_gesture.is_some()
                || cancelled_selection_stretch_claims_pointer;
            // Requests deferred to after UI borrows (same pattern as inspector).
            let mut gesture_refresh_preview = false;
            let mut gesture_restore_preview = false;
            let mut gesture_spawn_tool_preview: Option<(ToolKind, f32, Option<(usize, usize)>)> =
                None;
            if gain_env_editing && tab.selection_stretch_gesture.is_none() {
                if resp.hovered() {
                    hover_cursor = Some(egui::CursorIcon::Crosshair);
                }
                let value_to_y = |db: f32| frac_to_y(0.5 + db / (GAIN_ENV_DB_RANGE * 2.0));
                let y_to_value = |y: f32| {
                    ((y_to_frac(y) - 0.5) * GAIN_ENV_DB_RANGE * 2.0)
                        .clamp(-GAIN_ENV_DB_RANGE, GAIN_ENV_DB_RANGE)
                };
                let axis = CurveAxis {
                    value_to_y: &value_to_y,
                    y_to_value: &y_to_value,
                    sample_to_x: &curve_sample_to_x,
                    x_to_sample: &curve_x_to_sample,
                };
                let edit = curve_canvas::edit_breakpoint_curve(
                    ui,
                    &resp,
                    &mut tab.gain_env_points,
                    &mut tab.gain_env_drag,
                    &axis,
                );
                if edit.owns_pointer {
                    tool_gesture_active = true;
                    suppress_seek = true;
                }
                if edit.changed {
                    stop_playback = true;
                    tab.preview_overlay = None;
                    if tab.gain_env_points.is_empty() {
                        gesture_restore_preview = true;
                    } else {
                        tab.preview_audio_tool = Some(ToolKind::Gain);
                        gesture_refresh_preview = true;
                    }
                }
            }
            if pitch_env_editing && tab.selection_stretch_gesture.is_none() {
                if resp.hovered() {
                    hover_cursor = Some(egui::CursorIcon::Crosshair);
                }
                let value_to_y = |semi: f32| frac_to_y(0.5 + semi / (PITCH_SEMI_RANGE * 2.0));
                let y_to_value = |y: f32| {
                    ((y_to_frac(y) - 0.5) * PITCH_SEMI_RANGE * 2.0)
                        .clamp(-PITCH_SEMI_RANGE, PITCH_SEMI_RANGE)
                };
                let axis = CurveAxis {
                    value_to_y: &value_to_y,
                    y_to_value: &y_to_value,
                    sample_to_x: &curve_sample_to_x,
                    x_to_sample: &curve_x_to_sample,
                };
                let edit = curve_canvas::edit_breakpoint_curve(
                    ui,
                    &resp,
                    &mut tab.pitch_env_points,
                    &mut tab.pitch_env_drag,
                    &axis,
                );
                if edit.owns_pointer {
                    tool_gesture_active = true;
                    suppress_seek = true;
                }
                if edit.changed {
                    stop_playback = true;
                    tab.preview_overlay = None;
                    let has_shift = tab
                        .pitch_env_points
                        .iter()
                        .any(|(_, semitones)| semitones.abs() > 0.0001)
                        || tab.tool_state.pitch_semitones.abs() > 0.0001;
                    if has_shift {
                        tab.preview_audio_tool = Some(ToolKind::PitchShift);
                        gesture_refresh_preview = true;
                    } else {
                        gesture_restore_preview = true;
                    }
                }
            }
            // An EQ / Compressor / Limiter automation lane: its points are
            // part of the tool's draft, so the preview follows them like any
            // other setting (no stop, no restart).
            if let Some(param) = automation_param
                .filter(|_| wave_view_gestures && tab.selection_stretch_gesture.is_none())
            {
                if resp.hovered() {
                    hover_cursor = Some(egui::CursorIcon::Crosshair);
                }
                let st = tab.tool_state;
                let value_to_y = |v: f32| frac_to_y(param.to_frac(&st, v));
                let y_to_value = |y: f32| param.from_frac(&st, y_to_frac(y));
                let axis = CurveAxis {
                    value_to_y: &value_to_y,
                    y_to_value: &y_to_value,
                    sample_to_x: &curve_sample_to_x,
                    x_to_sample: &curve_x_to_sample,
                };
                let mut drag = tab.automation_drag;
                let lane = tool_automation::lane_mut(tab, param);
                let edit =
                    curve_canvas::edit_breakpoint_curve(ui, &resp, &mut lane.points, &mut drag, &axis);
                tab.automation_drag = drag;
                if edit.owns_pointer {
                    tool_gesture_active = true;
                    suppress_seek = true;
                }
            }
            // Pencil: Ctrl+drag draws. Plain interaction selects sample points,
            // drags a point/selected range vertically, or box-selects by time.
            let pencil_editing = wave_view_gestures
                && matches!(tab.active_tool, ToolKind::Pencil)
                && tab.pencil_draft.is_some();
            if pencil_editing && tab.selection_stretch_gesture.is_none() {
                tool_gesture_active = true;
                suppress_seek = true;
                let visible = tab.channel_view.visible_indices(tab.ch_samples.len());
                let use_mixdown =
                    tab.channel_view.mode == crate::app::types::ChannelViewMode::Mixdown
                        || visible.is_empty();
                let zoomed_in_enough = tab.samples_per_px <= 0.5;
                let pointer_context = |tab: &EditorTab, pos: egui::Pos2| {
                    let sample =
                        geom.x_to_display_sample(pos.x.clamp(wave_left, wave_left + wave_w));
                    // The lane under the pointer as drawn, scroll included.
                    let lane_idx = if use_mixdown {
                        0
                    } else {
                        lane_geom.lane_at_y(pos.y).min(visible.len().saturating_sub(1))
                    };
                    let channels = if use_mixdown {
                        (0..tab.ch_samples.len()).collect::<Vec<_>>()
                    } else {
                        vec![visible[lane_idx]]
                    };
                    let lane_rect = egui::Rect::from_min_size(
                        egui::pos2(wave_left, lane_geom.lane_top(lane_idx)),
                        egui::vec2(wave_w, lane_geom.lane_h),
                    );
                    let display_amp = crate::app::render::overlay::waveform_amp_from_y(
                        lane_rect,
                        tab.vertical_zoom,
                        tab.vertical_view_center,
                        pos.y,
                    );
                    let source_amp = (display_amp / scale.max(1.0e-6)).clamp(-4.0, 4.0);
                    let point_amp = tab
                        .preview_overlay
                        .as_ref()
                        .filter(|overlay| overlay.source_tool == ToolKind::Pencil)
                        .and_then(|overlay| {
                            if use_mixdown {
                                overlay
                                    .mixdown
                                    .as_ref()
                                    .and_then(|samples| samples.get(sample))
                                    .copied()
                            } else {
                                channels.first().and_then(|&channel| {
                                    overlay
                                        .channels
                                        .get(channel)
                                        .and_then(|samples| samples.get(sample))
                                        .copied()
                                })
                            }
                        })
                        .unwrap_or(0.0);
                    let point_y = Self::waveform_y_from_amp(
                        lane_rect,
                        tab.vertical_zoom,
                        tab.vertical_view_center,
                        (point_amp * scale).clamp(-1.0, 1.0),
                    );
                    let point_x = geom.sample_center_x(sample);
                    let point_hit = (pos.x - point_x).abs()
                        <= Self::waveform_sample_point_radius(tab.samples_per_px)
                            .unwrap_or(2.0)
                            .max(2.0)
                            + 3.0
                        && (pos.y - point_y).abs() <= 8.0;
                    (
                        sample,
                        lane_idx,
                        channels,
                        lane_rect,
                        source_amp,
                        point_hit,
                    )
                };

                if resp.hovered() && zoomed_in_enough {
                    let ctrl = ui.input(|input| input.modifiers.ctrl);
                    hover_cursor = if ctrl {
                        Some(egui::CursorIcon::Crosshair)
                    } else {
                        resp.hover_pos()
                            .map(|pos| pointer_context(tab, pos).5)
                            .filter(|hit| *hit)
                            .map(|_| egui::CursorIcon::ResizeVertical)
                            .or(Some(egui::CursorIcon::Crosshair))
                    };
                }

                if zoomed_in_enough && resp.drag_started_by(egui::PointerButton::Primary) {
                    if let Some(pos) = ui
                        .input(|input| input.pointer.press_origin())
                        .or_else(|| resp.interact_pointer_pos())
                    {
                        let ctrl = ui.input(|input| input.modifiers.ctrl);
                        let (sample, _, channels, _, amp, point_hit) =
                            pointer_context(tab, pos);
                        if ctrl {
                            if Self::editor_pencil_begin_stroke(tab, &channels) {
                                tab.pencil_last_point = Some((sample, amp));
                                if let Some(draft) = tab.pencil_draft.as_mut() {
                                    draft.selection = None;
                                    draft.drag = Some(PencilDragState::Draw);
                                }
                            }
                        } else if point_hit {
                            let selected = tab
                                .pencil_draft
                                .as_ref()
                                .and_then(|draft| draft.selection.as_ref())
                                .filter(|selection| {
                                    selection.channels == channels
                                        && selection.start <= sample
                                        && sample < selection.end
                                })
                                .cloned()
                                .unwrap_or(PencilPointSelection {
                                    start: sample,
                                    end: sample.saturating_add(1).min(tab.samples_len),
                                    channels: channels.clone(),
                                });
                            if Self::editor_pencil_begin_range_move(
                                tab,
                                &selected.channels,
                                selected.start,
                                selected.end,
                            ) {
                                if let Some(draft) = tab.pencil_draft.as_mut() {
                                    draft.selection = Some(selected);
                                    draft.drag =
                                        Some(PencilDragState::Move { origin_amp: amp });
                                }
                            }
                        } else if let Some(draft) = tab.pencil_draft.as_mut() {
                            draft.selection = Some(PencilPointSelection {
                                start: sample,
                                end: sample.saturating_add(1).min(tab.samples_len),
                                channels: channels.clone(),
                            });
                            draft.drag = Some(PencilDragState::Select {
                                anchor_sample: sample,
                                channels,
                            });
                        }
                    }
                }

                if zoomed_in_enough && resp.dragged_by(egui::PointerButton::Primary) {
                    let drag = tab
                        .pencil_draft
                        .as_ref()
                        .and_then(|draft| draft.drag.clone());
                    if let (Some(drag), Some(pos)) = (drag, resp.interact_pointer_pos()) {
                        match drag {
                            PencilDragState::Draw => {
                                // Releasing Ctrl immediately stops modifying the draft.
                                if ui.input(|input| input.modifiers.ctrl) {
                                    let (sample, _, _, _, amp, _) = pointer_context(tab, pos);
                                    let from = tab.pencil_last_point.unwrap_or((sample, amp));
                                    let channels = tab.pencil_stroke_channels.clone();
                                    if from.0 != sample || (from.1 - amp).abs() > f32::EPSILON {
                                        Self::editor_pencil_write_segment(
                                            tab,
                                            &channels,
                                            from,
                                            (sample, amp),
                                        );
                                        tab.pencil_last_point = Some((sample, amp));
                                        ui.ctx().request_repaint();
                                    }
                                }
                            }
                            PencilDragState::Move { origin_amp } => {
                                let (_, _, _, _, amp, _) = pointer_context(tab, pos);
                                if Self::editor_pencil_move_active_range(tab, amp - origin_amp) {
                                    ui.ctx().request_repaint();
                                }
                            }
                            PencilDragState::Select {
                                anchor_sample,
                                channels,
                            } => {
                                let (sample, _, _, _, _, _) = pointer_context(tab, pos);
                                if let Some(draft) = tab.pencil_draft.as_mut() {
                                    draft.selection = Some(PencilPointSelection {
                                        start: anchor_sample.min(sample),
                                        end: anchor_sample
                                            .max(sample)
                                            .saturating_add(1)
                                            .min(tab.samples_len),
                                        channels,
                                    });
                                    ui.ctx().request_repaint();
                                }
                            }
                        }
                    }
                }

                // A click only changes point selection. It never writes audio.
                if zoomed_in_enough
                    && resp.clicked_by(egui::PointerButton::Primary)
                    && !ui.input(|input| input.modifiers.ctrl)
                {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let (sample, _, channels, _, _, point_hit) =
                            pointer_context(tab, pos);
                        if let Some(draft) = tab.pencil_draft.as_mut() {
                            draft.selection = point_hit.then_some(PencilPointSelection {
                                start: sample,
                                end: sample.saturating_add(1).min(tab.samples_len),
                                channels,
                            });
                        }
                        ui.ctx().request_repaint();
                    }
                }

                if ui.input(|input| input.pointer.primary_released()) {
                    let completed_drag = tab
                        .pencil_draft
                        .as_mut()
                        .and_then(|draft| draft.drag.take());
                    if matches!(
                        completed_drag,
                        Some(PencilDragState::Draw | PencilDragState::Move { .. })
                    ) && Self::editor_pencil_finish_stroke(tab)
                    {
                        pending_pencil_preview_refresh = true;
                    }
                    tab.pencil_last_point = None;
                    tab.pencil_stroke_channels.clear();
                }
            }
            // PitchShift: draggable horizontal pitch line (up = higher).
            let pitch_line_tool = wave_view_gestures
                && matches!(tab.active_tool, ToolKind::PitchShift)
                && !tab.pitch_env_enabled;
            if pitch_line_tool && tab.selection_stretch_gesture.is_none() {
                let semi_to_frac = |semi: f32| 0.5 + semi / (PITCH_SEMI_RANGE * 2.0);
                let frac_to_semi = |frac: f32| (frac - 0.5) * PITCH_SEMI_RANGE * 2.0;
                let mut semi = tab.tool_state.pitch_semitones;
                if !semi.is_finite() {
                    semi = 0.0;
                }
                let line_y = frac_to_y(semi_to_frac(semi));
                let near_line = |pos: egui::Pos2| (pos.y - line_y).abs() <= 8.0;
                if pointer_over_waveform && tab.selection_stretch_gesture.is_none() {
                    if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                        if near_line(pos) || tab.pitch_drag_active {
                            hover_cursor = Some(egui::CursorIcon::ResizeVertical);
                        }
                    }
                }
                if resp.drag_started_by(egui::PointerButton::Primary) {
                    if let Some(pos) = ui
                        .input(|i| i.pointer.press_origin())
                        .or_else(|| resp.interact_pointer_pos())
                    {
                        if near_line(pos) {
                            tab.pitch_drag_active = true;
                        }
                    }
                }
                if tab.pitch_drag_active {
                    tool_gesture_active = true;
                    suppress_seek = true;
                    if resp.dragged_by(egui::PointerButton::Primary) {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            let next = frac_to_semi(y_to_frac(pos.y))
                                .clamp(-PITCH_SEMI_RANGE, PITCH_SEMI_RANGE);
                            tab.tool_state = ToolState {
                                pitch_semitones: next,
                                ..tab.tool_state
                            };
                        }
                    }
                    if resp.drag_stopped_by(egui::PointerButton::Primary) {
                        tab.pitch_drag_active = false;
                        let semi = tab.tool_state.pitch_semitones;
                        stop_playback = true;
                        if semi.abs() <= 0.0001 {
                            gesture_restore_preview = true;
                        } else {
                            let range = Self::editor_selected_range(tab);
                            tab.preview_audio_tool = Some(ToolKind::PitchShift);
                            gesture_spawn_tool_preview =
                                Some((ToolKind::PitchShift, semi, range));
                        }
                    }
                }
            }
            // Safety: never let a gesture grab outlive the pointer button
            // (e.g. release outside the window where drag_stopped is missed).
            if !ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary))
                && !resp.drag_stopped_by(egui::PointerButton::Primary)
            {
                tab.pitch_drag_active = false;
                tab.pitch_env_drag = None;
                tab.spectral_warp_drag = None;
            }
            // Resolve deferred gesture requests into the shared pending slots.
            if gesture_restore_preview {
                need_restore_preview = true;
            }
            if gesture_refresh_preview {
                request_preview_refresh = true;
            }
            // A canvas gesture that ended asks the preview engine for the
            // settings it left behind.
            if gesture_spawn_tool_preview.is_some() {
                request_preview_refresh = true;
            }
            let alt_now = ui.input(|i| i.modifiers.alt);
            let selection_snap_playhead_display = playhead_display_now.min(display_samples_len);
            let selection_snap_playhead_x = geom.sample_center_x(selection_snap_playhead_display);
            let selection_snap_radius_px = 8.0f32;
            let to_range_selection_display_sample = |x: f32| -> usize {
                let x = x.clamp(wave_left, wave_left + wave_w);
                if (x - selection_snap_playhead_x).abs() <= selection_snap_radius_px {
                    return selection_snap_playhead_display;
                }
                geom.x_to_display_sample(x)
            };
            // Right drag is dedicated to seek/playhead movement.
            // Shift+Right drag switches to range selection with button-down anchor.
            if pointer_over_waveform
                && !world_f0_editing
                && !tool_gesture_active
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
                        if let Some(pos) = ui.input(|i| i.pointer.press_origin())
                            .or_else(|| resp.interact_pointer_pos())
                            .or_else(|| ui.input(|i| i.pointer.hover_pos()))
                        {
                            let samp = to_range_selection_display_sample(pos.x);
                            Self::editor_set_selection_from_anchor(tab, samp, samp);
                            suppress_seek = true;
                        }
                    }
                }
                if right_dragging {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        match tab.right_drag_mode.unwrap_or(if shift_now {
                            RightDragMode::SelectRange
                        } else {
                            RightDragMode::Seek
                        }) {
                            RightDragMode::Seek => {
                                let samp = to_display_sample(pos.x);
                                request_seek = Some(map_display_to_audio(tab, samp));
                                suppress_seek = true;
                            }
                            RightDragMode::SelectRange => {
                                let samp = to_range_selection_display_sample(pos.x);
                                let anchor = Self::editor_selection_anchor_or(
                                    tab,
                                    selection_snap_playhead_display,
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
            // Edge Fade owns a narrow draggable handle at each file edge.
            // Dragging updates only the draft; audio changes on Apply.
            if (pointer_over_waveform || tab.fade_drag_edge.is_some())
                && !world_f0_editing
                && matches!(tab.active_tool, ToolKind::Fade)
                && display_samples_len > 0
            {
                let pointer_pressed =
                    ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));
                let pointer_down =
                    ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary));
                let pointer_released =
                    ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary));
                let sr = tab.buffer_sample_rate.max(1) as f32;
                let to_x = |sample: usize| geom.sample_boundary_x(sample);
                let fade_in_samples =
                    ((tab.tool_state.fade_in_ms.max(0.0) / 1000.0) * sr).round() as usize;
                let fade_out_samples =
                    ((tab.tool_state.fade_out_ms.max(0.0) / 1000.0) * sr).round() as usize;
                let fade_in_x = to_x(fade_in_samples.min(display_samples_len));
                let fade_out_x =
                    to_x(display_samples_len.saturating_sub(fade_out_samples));

                if pointer_pressed && tab.fade_drag_edge.is_none() {
                    if let Some(pos) = ui
                        .input(|i| i.pointer.press_origin())
                        .or_else(|| resp.interact_pointer_pos())
                    {
                        let hit_radius = 10.0;
                        let mut nearest: Option<(FadeDragEdge, f32)> = None;
                        for (edge, handle_x) in [
                            (FadeDragEdge::In, fade_in_x),
                            (FadeDragEdge::Out, fade_out_x),
                        ] {
                            let distance = (pos.x - handle_x).abs();
                            if distance <= hit_radius
                                && nearest
                                    .as_ref()
                                    .is_none_or(|(_, best)| distance < *best)
                            {
                                nearest = Some((edge, distance));
                            }
                        }
                        if let Some((edge, _)) = nearest {
                            tab.fade_drag_edge = Some(edge);
                            tool_gesture_active = true;
                            suppress_seek = true;
                            stop_playback = true;
                        }
                    }
                }

                if pointer_down {
                    if let Some(edge) = tab.fade_drag_edge {
                        if let Some(pos) = ui
                            .input(|i| i.pointer.hover_pos())
                            .or_else(|| resp.interact_pointer_pos())
                        {
                            let sample = geom
                                .x_to_display_sample(pos.x.clamp(wave_left, wave_left + wave_w));
                            let duration_samples = match edge {
                                FadeDragEdge::In => sample,
                                FadeDragEdge::Out => {
                                    display_samples_len.saturating_sub(sample)
                                }
                            };
                            let duration_ms = duration_samples as f32 / sr * 1000.0;
                            match edge {
                                FadeDragEdge::In => {
                                    tab.tool_state.fade_in_ms = duration_ms;
                                }
                                FadeDragEdge::Out => {
                                    tab.tool_state.fade_out_ms = duration_ms;
                                }
                            }
                        }
                        tool_gesture_active = true;
                        suppress_seek = true;
                    }
                }

                if pointer_released {
                    if tab.fade_drag_edge.take().is_some() {
                        tool_gesture_active = true;
                        suppress_seek = true;
                        if tab.preview_audio_tool == Some(ToolKind::Fade)
                            || tab
                                .preview_overlay
                                .as_ref()
                                .is_some_and(|overlay| overlay.source_tool == ToolKind::Fade)
                        {
                            request_preview_refresh = true;
                        }
                    }
                } else if !pointer_down {
                    tab.fade_drag_edge = None;
                }
            }
            if pointer_over_waveform
                && !world_f0_editing
                && matches!(tab.active_tool, ToolKind::LoopEdit)
                && display_samples_len > 0
            {
                let pointer_down = ui.input(|i| i.pointer.primary_down());
                let pointer_released = ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary));
                // Kept across the reset below: the double click that zooms is
                // only recognisable once the button comes back up, by which
                // point the drag state has to be gone.
                let armed_marker = tab.dragging_marker;
                let armed_and_moved = tab.loop_drag_moved;
                if pointer_released || !pointer_down {
                    tab.dragging_marker = None;
                    tab.loop_drag_moved = false;
                }
                // A second click on a handle, in place, zooms in around it --
                // the gesture for "let me look at this edge properly". It can
                // never collide with the drag, which by definition has moved.
                if pointer_released && !armed_and_moved {
                    if let Some(marker) = armed_marker {
                        let release_pos = ui
                            .input(|i| i.pointer.interact_pos())
                            .or_else(|| ui.input(|i| i.pointer.hover_pos()));
                        let repeat = release_pos.is_some_and(|pos| {
                            crate::app::helpers::note_repeated_click(
                                &mut self.editor_loop_handle_last_click,
                                pos,
                            )
                        });
                        if repeat {
                            if let Some((a, b)) = Self::normalized_loop_range(tab.loop_region) {
                                zoom_around_loop_point = Some(match marker {
                                    MarkerKind::A => a,
                                    MarkerKind::B => b,
                                });
                            }
                            suppress_seek = true;
                        }
                    }
                }
                if pointer_down {
                    // Arm on the press, and only on the press.
                    //
                    // This used to arm from `button_down` — true on every frame
                    // the button is held — and then fall through and move the
                    // loop point in the same frame, so a plain click on a loop
                    // edge dragged it to the clicked pixel before the pointer
                    // had moved at all. Clicking a loop edge to put the playhead
                    // on it is the everyday gesture; moving the edge is the
                    // rarer one. Now the click seeks and only a drag moves.
                    if tab.dragging_marker.is_none()
                        && ui.input(|i| i.pointer.primary_pressed())
                        && tab.selection_edge_drag_anchor.is_none()
                        && tab.selection_stretch_gesture.is_none()
                        && !tab.selection_stretch_cancel_until_release
                    {
                        if let Some(pos) = ui
                            .input(|i| i.pointer.press_origin())
                            .or_else(|| ui.input(|i| i.pointer.hover_pos()))
                        {
                            // The Time Stretch grip owns the top of this same
                            // line, so a press inside its band is never a loop
                            // drag — even when the stretch itself declined it.
                            let grip_owns_press = view_mode == ViewMode::Waveform
                                && in_selection_stretch_band(rect.top(), pos.y);
                            if !grip_owns_press {
                                if let Some((a, b)) = Self::normalized_loop_range(tab.loop_region) {
                                    // Unclamped, and only for edges actually on
                                    // screen: `sample_boundary_x` pins an edge
                                    // scrolled off to the left onto the canvas
                                    // border, where it answered presses nowhere
                                    // near the loop.
                                    let mut candidates: Vec<(f32, usize)> = Vec::new();
                                    if geom.contains_boundary(a) {
                                        candidates.push((geom.sample_boundary_x_unclamped(a), 0));
                                    }
                                    if geom.contains_boundary(b) {
                                        candidates.push((geom.sample_boundary_x_unclamped(b), 1));
                                    }
                                    // Nearest, not first. The `if / else if`
                                    // this replaces handed the start every
                                    // press on a loop shorter than two grab
                                    // radii, so the end could not be grabbed.
                                    tab.dragging_marker = nearest_handle(
                                        &candidates,
                                        pos.x,
                                        LOOP_EDGE_GRAB_RADIUS,
                                    )
                                    .map(|which| {
                                        if which == 0 {
                                            MarkerKind::A
                                        } else {
                                            MarkerKind::B
                                        }
                                    });
                                }
                            }
                        }
                    }
                    if let Some(marker) = tab.dragging_marker {
                        // Handles are drawn on the sample *boundary* but
                        // x -> sample is centre-based, so feeding the press
                        // position straight back would shift the edge by a
                        // sample on a bare click. Latched, because a drag that
                        // wandered back through its own start would otherwise
                        // hand the release back to the seek handler.
                        if !tab.loop_drag_moved {
                            tab.loop_drag_moved = ui
                                .input(|i| i.pointer.press_origin())
                                .zip(ui.input(|i| i.pointer.hover_pos()))
                                .is_some_and(|(origin, pos)| (pos.x - origin.x).abs() >= 0.5);
                        }
                        if tab.loop_drag_moved {
                            if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                                if let Some((mut a, mut b)) =
                                    Self::normalized_loop_range(tab.loop_region)
                                {
                                    // One drag, one undo step: opened here,
                                    // pushed when the pointer lets go.
                                    Self::annotation_begin(tab, "Loop Edge");
                                    let samp = Self::loop_edge_snap_sample(
                                        tab,
                                        &geom,
                                        self.zero_cross_epsilon,
                                        alt_now,
                                        pos.x,
                                    );
                                    match marker {
                                        MarkerKind::A => a = samp.min(b),
                                        MarkerKind::B => b = samp.max(a),
                                    }
                                    tab.loop_region = Some((a, b));
                                    Self::update_loop_markers_dirty(tab);
                                    apply_pending_loop = true;
                                }
                            }
                            suppress_seek = true;
                        }
                    }
                }
            }
            // Spectral views: dragging also selects a frequency band
            // (time-frequency rectangle, RX-style). These helpers convert
            // between screen Y within a channel lane and Hz using the same
            // mapping as the axis ticks and the viewport renderer.
            let spec_freq_view = matches!(
                view_mode,
                ViewMode::Spectrogram | ViewMode::Log | ViewMode::Mel
            );
            let spec_axis_sr = self
                .spectro_cache
                .get(&tab.path)
                .and_then(|specs| specs.first())
                .map(|spec| spec.sample_rate)
                // Before the spectrogram exists: the tab's buffer rate.
                .unwrap_or(tab.buffer_sample_rate);
            let spec_axis_max_freq = Self::editor_spec_axis_max_freq(&self.spectro_cfg, spec_axis_sr);
            let spec_mel_scale = self.spectro_cfg.mel_scale;
            let (spec_vis_min, spec_vis_max) = Self::editor_vertical_range_for_view(
                view_mode,
                tab.vertical_zoom,
                tab.vertical_view_center,
                &self.spectro_cfg,
            );
            let spec_lane_at_y = move |y: f32| -> usize { lane_geom.lane_at_y(y) };
            let spec_y_to_freq = {
                let lane_h = lane_h.max(1.0);
                move |y: f32, lane: usize| -> f32 {
                    let lane_top = lane_geom.lane_top(lane);
                    let frac_local = 1.0 - ((y - lane_top) / lane_h).clamp(0.0, 1.0);
                    let frac = spec_vis_min + frac_local * (spec_vis_max - spec_vis_min);
                    Self::editor_spec_frac_to_freq(
                        view_mode,
                        frac,
                        spec_axis_max_freq,
                        spec_mel_scale,
                    )
                }
            };
            // ---- Spectral Warp input (Spec/Log views): while enabled it owns
            // the pointer — drag pushes frequencies (liquify-style), grabbing
            // an existing arrow adjusts it, double/right-click removes it.
            if spectral_warp_editing {
                suppress_seek = true;
                if resp.hovered() {
                    hover_cursor = Some(egui::CursorIcon::Crosshair);
                }
                let spec_freq_to_y = |hz: f32, lane: usize| -> f32 {
                    let frac = Self::editor_spec_freq_to_frac(
                        view_mode,
                        hz,
                        spec_axis_max_freq,
                        spec_mel_scale,
                    );
                    let frac_local = ((frac - spec_vis_min)
                        / (spec_vis_max - spec_vis_min).max(1e-6))
                    .clamp(0.0, 1.0);
                    let lane_top = lane_geom.lane_top(lane);
                    lane_top + (1.0 - frac_local) * lane_h.max(1.0)
                };
                let hit_radius = 10.0f32;
                let nearest_warp_point = |points: &[crate::app::types::SpectralWarpPoint],
                                          pos: egui::Pos2|
                 -> Option<usize> {
                    let lane = spec_lane_at_y(pos.y);
                    let mut best: Option<(usize, f32)> = None;
                    for (idx, p) in points.iter().enumerate() {
                        let x = geom.sample_center_x(p.sample.min(display_samples_len));
                        let d0 = egui::pos2(x, spec_freq_to_y(p.freq_hz, lane)).distance(pos);
                        let d1 = egui::pos2(x, spec_freq_to_y(p.freq_hz + p.delta_hz, lane))
                            .distance(pos);
                        let d = d0.min(d1);
                        if d <= hit_radius && best.map(|(_, bd)| d < bd).unwrap_or(true) {
                            best = Some((idx, d));
                        }
                    }
                    best.map(|(i, _)| i)
                };
                let mut warp_changed = false;
                let double_clicked = resp.double_clicked_by(egui::PointerButton::Primary);
                let right_clicked = resp.clicked_by(egui::PointerButton::Secondary);
                if double_clicked || right_clicked {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        if let Some(idx) = nearest_warp_point(&tab.spectral_warp_points, pos) {
                            tab.spectral_warp_points.remove(idx);
                            tab.spectral_warp_drag = None;
                            warp_changed = true;
                        }
                    }
                }
                if !double_clicked
                    && resp.drag_started_by(egui::PointerButton::Primary)
                    && tab.spectral_warp_drag.is_none()
                {
                    if let Some(pos) = ui
                        .input(|i| i.pointer.press_origin())
                        .or_else(|| resp.interact_pointer_pos())
                    {
                        if let Some(idx) = nearest_warp_point(&tab.spectral_warp_points, pos) {
                            tab.spectral_warp_drag = Some(idx);
                        } else {
                            let sample = geom
                                .x_to_display_sample(pos.x.clamp(wave_left, wave_left + wave_w));
                            let lane = spec_lane_at_y(pos.y);
                            let hz = spec_y_to_freq(pos.y, lane).max(0.0);
                            tab.spectral_warp_points.push(
                                crate::app::types::SpectralWarpPoint {
                                    sample,
                                    freq_hz: hz,
                                    delta_hz: 0.0,
                                },
                            );
                            tab.spectral_warp_drag = Some(tab.spectral_warp_points.len() - 1);
                        }
                    }
                }
                if resp.dragged_by(egui::PointerButton::Primary) {
                    if let (Some(idx), Some(pos)) =
                        (tab.spectral_warp_drag, resp.interact_pointer_pos())
                    {
                        if let Some(p) = tab.spectral_warp_points.get_mut(idx) {
                            let lane = spec_lane_at_y(pos.y);
                            let target = spec_y_to_freq(pos.y, lane).max(0.0);
                            p.delta_hz = target - p.freq_hz;
                        }
                    }
                }
                if ui.input(|i| i.pointer.primary_released()) {
                    if tab.spectral_warp_drag.take().is_some() {
                        warp_changed = true;
                    }
                }
                if warp_changed {
                    stop_playback = true;
                    if Self::editor_spectral_warp_ready(tab) {
                        // Render + audition the new warp right away.
                        pending_spectral_warp_preview = true;
                    } else if tab.preview_audio_tool
                        == Some(crate::app::types::ToolKind::SpectralWarp)
                        || tab.preview_overlay.is_some()
                    {
                        need_restore_preview = true;
                    }
                }
            }
            // ---- Spectral Brush input (Spec/Log views): while enabled it
            // owns the pointer — dragging paints attenuation stamps along the
            // stroke; the preview renders on release.
            if spectral_brush_editing {
                suppress_seek = true;
                if resp.hovered() {
                    hover_cursor = Some(egui::CursorIcon::Crosshair);
                }
                let painting = resp.drag_started_by(egui::PointerButton::Primary)
                    || resp.dragged_by(egui::PointerButton::Primary);
                if painting {
                    let pos = if resp.drag_started_by(egui::PointerButton::Primary) {
                        ui.input(|i| i.pointer.press_origin())
                            .or_else(|| resp.interact_pointer_pos())
                    } else {
                        resp.interact_pointer_pos()
                    };
                    if let Some(pos) = pos {
                        let sample =
                            geom.x_to_display_sample(pos.x.clamp(wave_left, wave_left + wave_w));
                        let lane = spec_lane_at_y(pos.y);
                        let hz = spec_y_to_freq(pos.y, lane).max(0.0);
                        let st = tab.tool_state;
                        let sigma_t_ms = st.brush_time_radius_ms.max(1.0);
                        let sigma_f_hz = st.brush_freq_radius_hz.max(1.0);
                        // Decimate the stroke: a new stamp closer than half a
                        // sigma (time AND frequency) to the previous one adds
                        // nothing audible, only processing cost.
                        let sigma_t_samples =
                            (sigma_t_ms / 1000.0) * tab.buffer_sample_rate.max(1) as f32;
                        let accept = tab
                            .spectral_brush_last
                            .map(|(ls, lf)| {
                                let dt = (sample as f32 - ls as f32).abs();
                                let df = (hz - lf).abs();
                                dt >= sigma_t_samples * 0.5 || df >= sigma_f_hz * 0.5
                            })
                            .unwrap_or(true);
                        if accept {
                            tab.spectral_brush_stamps.push(
                                crate::app::types::SpectralBrushStamp {
                                    sample,
                                    freq_hz: hz,
                                    cut_db: st.brush_cut_db.max(0.0),
                                    time_sigma_ms: sigma_t_ms,
                                    freq_sigma_hz: sigma_f_hz,
                                },
                            );
                            tab.spectral_brush_last = Some((sample, hz));
                        }
                    }
                }
                if ui.input(|i| i.pointer.primary_released())
                    && tab.spectral_brush_last.take().is_some()
                {
                    stop_playback = true;
                    if Self::editor_spectral_brush_ready(tab) {
                        // Render + audition the stroke right away.
                        pending_spectral_brush_preview = true;
                    }
                }
            }
            // Update the Waveform-only selection Time Stretch gesture. Pointer
            // position is read globally so releasing outside the canvas still
            // commits the last valid clamped target. The real selection stays
            // unchanged until the worker result is adopted.
            if let Some(mut gesture) = tab.selection_stretch_gesture {
                tool_gesture_active = true;
                suppress_seek = true;
                let pointer_down = ui.input(|input| input.pointer.primary_down());
                let pointer_released = ui.input(|input| input.pointer.primary_released());
                if pointer_down || pointer_released {
                    if let Some(pos) = ui.input(|input| input.pointer.hover_pos()) {
                        let delta_px = pos.x - gesture.press_pointer_x;
                        if delta_px.abs() >= 0.5 {
                            gesture.moved = true;
                        }
                        gesture.target_len = selection_stretch_target_len(
                            gesture.source_range.1 - gesture.source_range.0,
                            gesture.edge,
                            delta_px,
                            geom.spp,
                        );
                        tab.selection_stretch_gesture = Some(gesture);
                    }
                }
                if pointer_released {
                    if let Some(completed) = tab.selection_stretch_gesture.take() {
                        let source_len = completed.source_range.1 - completed.source_range.0;
                        if completed.moved && completed.target_len != source_len {
                            let rate =
                                selection_stretch_rate(source_len, completed.target_len);
                            pending_selection_stretch_apply = Some((
                                rate,
                                completed.source_range,
                                EditorApplyViewportRestore {
                                    dragged_edge: completed.edge,
                                    fixed_screen_offset_px: completed.fixed_screen_offset_px,
                                    samples_per_px: geom.spp,
                                },
                            ));
                            need_restore_preview = true;
                        }
                    }
                } else if !pointer_down {
                    // No release event means focus/input capture was lost.
                    tab.selection_stretch_gesture = None;
                }
            }

            // Grab the selection's edge line — anywhere below the Time Stretch
            // grip at the top — to lengthen or shorten the range. This moves
            // nothing but the range: no resample, no undo step, no worker.
            // Tool-independent, like the selection itself, and placed before
            // the drag-select block below so a grab on an existing edge wins
            // over starting a fresh range on the same press.
            if (pointer_over_waveform || tab.selection_edge_drag_anchor.is_some())
                && !world_f0_editing
                && !tool_gesture_active
                && display_samples_len > 0
            {
                let sel = tab.selection.map(|(a0, b0)| {
                    if a0 <= b0 {
                        (a0, b0)
                    } else {
                        (b0, a0)
                    }
                });
                match sel.filter(|&(a, b)| b > a) {
                    Some((sel_s, sel_e)) => {
                        // Candidates are (handle x, the anchor that stays put).
                        let mut candidates: Vec<(f32, usize)> = Vec::new();
                        if geom.contains_boundary(sel_s) {
                            candidates.push((geom.sample_boundary_x_unclamped(sel_s), sel_e));
                        }
                        if geom.contains_boundary(sel_e) {
                            candidates.push((geom.sample_boundary_x_unclamped(sel_e), sel_s));
                        }

                        // Grab on the raw button press, not `drag_started_by`:
                        // a handle has to respond without the drag threshold's
                        // worth of movement. The Time Stretch grip owns the top
                        // of the same line, so a press inside its band is never
                        // a resize — not even when the stretch itself declined
                        // it. `dragging_marker` is tested here rather than on
                        // the block, so the clear-on-release branch below stays
                        // reachable whatever else armed in the meantime.
                        if tab.selection_edge_drag_anchor.is_none()
                            && tab.dragging_marker.is_none()
                            && ui.input(|i| i.pointer.primary_pressed())
                        {
                            if let Some(pos) = ui
                                .input(|i| i.pointer.press_origin())
                                .or_else(|| ui.input(|i| i.pointer.hover_pos()))
                            {
                                let grip_owns_press = view_mode == ViewMode::Waveform
                                    && in_selection_stretch_band(rect.top(), pos.y);
                                if !grip_owns_press {
                                    if let Some(anchor) = nearest_handle(
                                        &candidates,
                                        pos.x,
                                        SELECTION_EDGE_GRAB_RADIUS,
                                    ) {
                                        tab.selection_edge_drag_anchor = Some(anchor);
                                    }
                                }
                            }
                        }

                        if let Some(anchor) = tab.selection_edge_drag_anchor {
                            if ui.input(|i| i.pointer.primary_down()) {
                                // Only once the pointer has actually moved.
                                // Handles are drawn on the sample *boundary*
                                // but x -> sample is centre-based, so feeding
                                // the press position straight back would shift
                                // the edge by a sample on a bare click.
                                let moved = ui
                                    .input(|i| i.pointer.press_origin())
                                    .zip(ui.input(|i| i.pointer.hover_pos()))
                                    .is_none_or(|(origin, pos)| (pos.x - origin.x).abs() >= 0.5);
                                if let Some(pos) =
                                    ui.input(|i| i.pointer.hover_pos()).filter(|_| moved)
                                {
                                    let raw = to_range_selection_display_sample(pos.x);
                                    let samp = if alt_now {
                                        zc_snap_nearest(&tab.ch_samples, tab.buffer_sample_rate, self.zero_cross_epsilon, raw)
                                    } else {
                                        raw
                                    };
                                    Self::editor_set_selection_from_anchor(tab, anchor, samp);
                                }
                            } else {
                                tab.selection_edge_drag_anchor = None;
                            }
                            // Held past the release frame as well: a grab with
                            // no movement is reported as a click, and the
                            // click handler would clear the whole selection.
                            tool_gesture_active = true;
                            suppress_seek = true;
                        }
                    }
                    None => tab.selection_edge_drag_anchor = None,
                }
            }

            // Drag to select a range (independent of tool), unless we are dragging markers.
            // Alt+Drag: both endpoints snap to nearest zero crossing.
            // Ctrl+Drag: push current selection to extra_selections and start a new one.
            if pointer_over_waveform
                && !world_f0_editing
                && !tool_gesture_active
                && display_samples_len > 0
                && tab.dragging_marker.is_none()
            {
                let drag_started = resp.drag_started_by(egui::PointerButton::Primary);
                let dragging = resp.dragged_by(egui::PointerButton::Primary);
                let drag_released = resp.drag_stopped_by(egui::PointerButton::Primary);
                let ctrl_now = ui.input(|i| i.modifiers.ctrl);
                // Ctrl+click on the spectrogram: harmonic action at the
                // clicked (time, frequency) point.
                if spec_freq_view && ctrl_now && resp.clicked_by(egui::PointerButton::Primary) {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let samp = to_range_selection_display_sample(pos.x);
                        let lane = spec_lane_at_y(pos.y);
                        let hz = spec_y_to_freq(pos.y, lane).max(20.0);
                        pending_harmonic_click = Some((samp, hz));
                        suppress_seek = true;
                    }
                }
                if drag_started {
                    if !ctrl_now {
                        tab.extra_selections.clear();
                    } else if let Some(sel) = tab.selection {
                        let (a, b) = if sel.0 <= sel.1 { sel } else { (sel.1, sel.0) };
                        if b > a {
                            tab.extra_selections.push((a, b));
                        }
                    }
                    if let Some(pos) = ui.input(|i| i.pointer.press_origin())
                        .or_else(|| resp.interact_pointer_pos())
                    {
                        let raw = to_range_selection_display_sample(pos.x);
                        let samp = if alt_now {
                            zc_snap_nearest(&tab.ch_samples, tab.buffer_sample_rate, self.zero_cross_epsilon, raw)
                        } else {
                            raw
                        };
                        tab.selection_anchor_sample = Some(samp);
                        if spec_freq_view {
                            let lane = spec_lane_at_y(pos.y);
                            tab.freq_selection_drag = Some((lane, spec_y_to_freq(pos.y, lane)));
                        } else {
                            // Wave view drags select time only: reset any
                            // frequency band from a previous spectral drag.
                            tab.freq_selection = None;
                            tab.freq_selection_drag = None;
                        }
                    }
                }
                if dragging {
                    let anchor = tab
                        .selection_anchor_sample
                        .or_else(|| {
                            ui.input(|i| i.pointer.press_origin())
                                .map(|pos| to_range_selection_display_sample(pos.x))
                        })
                        .or_else(|| {
                            resp.interact_pointer_pos()
                                .map(|pos| to_range_selection_display_sample(pos.x))
                        });
                    if tab.selection_anchor_sample.is_none() {
                        tab.selection_anchor_sample = anchor;
                    }
                    if let (Some(anchor), Some(pos)) = (anchor, resp.interact_pointer_pos()) {
                        let raw = to_range_selection_display_sample(pos.x);
                        let samp = if alt_now {
                            zc_snap_nearest(&tab.ch_samples, tab.buffer_sample_rate, self.zero_cross_epsilon, raw)
                        } else {
                            raw
                        };
                        Self::editor_set_selection_from_anchor(tab, anchor, samp);
                        if spec_freq_view {
                            if let Some((lane, anchor_hz)) = tab.freq_selection_drag {
                                let cur_hz = spec_y_to_freq(pos.y, lane);
                                let (lo, hi) = if anchor_hz <= cur_hz {
                                    (anchor_hz, cur_hz)
                                } else {
                                    (cur_hz, anchor_hz)
                                };
                                let lo_frac = Self::editor_spec_freq_to_frac(
                                    view_mode,
                                    lo,
                                    spec_axis_max_freq,
                                    spec_mel_scale,
                                );
                                let hi_frac = Self::editor_spec_freq_to_frac(
                                    view_mode,
                                    hi,
                                    spec_axis_max_freq,
                                    spec_mel_scale,
                                );
                                // Dragging edge-to-edge over the whole axis
                                // means "all frequencies" (time-only range).
                                tab.freq_selection = if lo_frac <= 0.002 && hi_frac >= 0.998 {
                                    None
                                } else {
                                    Some((lo, hi))
                                };
                            }
                        }
                        suppress_seek = true;
                    }
                }
                if drag_released {
                    tab.freq_selection_drag = None;
                }
            }
            // Selection vs Seek with primary button click
            if !suppress_seek {
                if resp.clicked_by(egui::PointerButton::Primary) {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let x = pos.x.clamp(wave_left, wave_left + wave_w);
                        let pos_samp = geom.x_to_display_sample(x);
                        let shift_now = ui.input(|i| i.modifiers.shift);
                        if shift_now {
                            let anchor =
                                Self::editor_selection_anchor_or(tab, selection_snap_playhead_display);
                            let snapped_samp = to_range_selection_display_sample(x);
                            Self::editor_set_selection_from_anchor(tab, anchor, snapped_samp);
                        } else {
                            tab.selection = None;
                            tab.freq_selection = None;
                            tab.freq_selection_drag = None;
                            tab.extra_selections.clear();
                            tab.selection_anchor_sample = None;
                            tab.right_drag_mode = None;
                            request_seek = Some(map_display_to_audio(tab, pos_samp));
                        }
                    }
                }
            }

            // Right-click the canvas to point a comment at what is on screen.
            //
            // This does not fight the right-button gestures above. egui opens a
            // context menu on `secondary_clicked` -- a click, not a drag -- so
            // right-drag still seeks and Shift+right-drag still selects, and
            // neither opens this. The two right-button gestures that *are*
            // clicks (the WORLD F0 pencil's erase and Spectral Warp's
            // delete-nearest-point) are shut out by the same pair of flags the
            // seek block uses.
            //
            // The closure runs every frame the menu is open, so it only reads
            // values already computed for this frame and hands the choice back
            // through `pending_comment_ref`; the work happens once, after the
            // tab borrow ends (see the note at the top of this function).
            if !world_f0_editing && !tool_gesture_active {
                let rate = tab.buffer_sample_rate.max(1) as f64;
                let selection_anchor = tab
                    .selection
                    .filter(|(a, b)| a != b)
                    .map(|(a, b)| crate::app::comments::CommentAnchor {
                        start_sec: a.min(b) as f64 / rate,
                        end_sec: Some(a.max(b) as f64 / rate),
                        freq_hz: tab.freq_selection,
                    });
                // The editor draws a playhead whether or not the transport is
                // loaded, so a reference to "here" must not go dead just
                // because nothing is playing.
                let playhead_sec =
                    transport_source_sec.or(Some(playhead_display_now as f64 / rate));
                // `tab_path` and `tab_file_name` were built once, before the
                // tab borrow: naming the file here would allocate on every
                // frame the canvas draws, not just while the menu is open.
                let path = &tab_path;
                resp.context_menu(|ui| {
                    ui.label(egui::RichText::new("Comment reference").small().weak());
                    if let Some(anchor) = crate::app::ui::comments::ui_comment_reference_items(
                        ui,
                        &tab_file_name,
                        playhead_sec,
                        selection_anchor,
                    ) {
                        pending_comment_ref = Some((path.clone(), anchor));
                    }
                });
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
            // A dynamics tool's lines and the gain its render takes, drawn
            // on the audio itself.
            let dynamics_overlay = if show_waveform {
                crate::app::render::dynamics_overlay::DynamicsOverlay::for_tab(tab)
            } else {
                None
            };

            // Draw per-channel lanes with dB grid and playhead
            waveform_render_started = Some(std::time::Instant::now());
            if show_waveform {
            for lane_idx in visible_lanes.clone() {
                let channel_index = if use_mixdown {
                    None
                } else {
                    visible_channels.get(lane_idx).copied()
                };
                let lane_top = lane_geom.lane_top(lane_idx);
                let lane_rect = egui::Rect::from_min_size(egui::pos2(wave_left, lane_top), egui::vec2(wave_w, lane_h));
                let waveform_columns =
                    ov::compute_waveform_device_columns(lane_rect, pixels_per_point);
                // The amplitude-0 line, and the -6/-12 dBFS grid around it.
                //
                // These used to be the same colour and the same width, so the
                // one line that says where the waveform is centred was
                // indistinguishable from the two that are only a scale. The
                // centre line is now the brighter of the two, in the same
                // family as the zero lines the amplitude and time navigators
                // already draw, and the grid steps back to let it read.
                let dbs = [-6.0f32, -12.0f32];
                let center_y =
                    Self::waveform_center_y(lane_rect, tab.vertical_zoom, tab.vertical_view_center);
                painter.line_segment(
                    [
                        egui::pos2(lane_rect.left(), center_y),
                        egui::pos2(lane_rect.right(), center_y),
                    ],
                    egui::Stroke::new(WAVE_CENTER_LINE_W, WAVE_CENTER_LINE_COL),
                );
                if let Some(overlay) = dynamics_overlay.as_ref() {
                    overlay.draw_lane_lines(&painter, lane_rect, |amp| {
                        Self::waveform_y_from_amp(lane_rect, tab.vertical_zoom, tab.vertical_view_center, amp)
                    });
                }
                // The gutter is left of the waveform area, so this never lands
                // on the trace or on a marker.
                let gutter_font = TextStyle::Monospace.resolve(ui.style());
                painter.text(
                    egui::pos2(rect.left() + 2.0, center_y),
                    egui::Align2::LEFT_CENTER,
                    "0",
                    gutter_font.clone(),
                    WAVE_CENTER_LABEL_COL,
                );
                let mut db_label_ys = [(0.0f32, 0.0f32); 2];
                for (slot, &db) in db_label_ys.iter_mut().zip(dbs.iter()) {
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
                    painter.line_segment([egui::pos2(lane_rect.left(), y0), egui::pos2(lane_rect.right(), y0)], egui::Stroke::new(1.0_f32, WAVE_DB_GRID_COL));
                    painter.line_segment([egui::pos2(lane_rect.left(), y1), egui::pos2(lane_rect.right(), y1)], egui::Stroke::new(1.0_f32, WAVE_DB_GRID_COL));
                    *slot = (db, y0);
                }
                // Labels on the left gutter, nearest the "0" first, each only
                // where it clears the one before it and stays in its own lane.
                // At the 44 px a lane keeps once the lanes scroll, the -6 and
                // -12 dB labels would otherwise sit on top of the "0", of each
                // other and of the neighbouring lane's labels.
                let gutter_row_h = ui.text_style_height(&TextStyle::Monospace);
                db_label_ys.sort_by(|a, b| {
                    (a.1 - center_y).abs().total_cmp(&(b.1 - center_y).abs())
                });
                let mut last_label_y = center_y;
                for (db, y) in db_label_ys {
                    let clear_of_last = (y - last_label_y).abs() >= gutter_row_h;
                    let inside_lane = y - gutter_row_h * 0.5 >= lane_rect.top();
                    if clear_of_last && inside_lane {
                        painter.text(egui::pos2(rect.left() + 2.0, y), egui::Align2::LEFT_CENTER, format!("{db:.0} dB"), gutter_font.clone(), Color32::GRAY);
                        last_label_y = y;
                    }
                }

                if visible_len > 0 {
                    let draw_from_overview = Self::editor_draws_overview_only(tab)
                        && !tab.loading_waveform_minmax.is_empty();
                    let (wave_lod, lane_query_ms, lane_draw_ms) = if draw_from_overview {
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
                                    matches!(overlay.source_tool, ToolKind::TimeStretch | ToolKind::Speed);
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
                                    let is_time_stretch = matches!(overlay.source_tool, ToolKind::TimeStretch | ToolKind::Speed);
                                    let unwrap_preview = matches!(overlay.source_tool, ToolKind::LoopEdit)
                                        && overlay_total > base_total
                                        && tab.tool_state.loop_apply_unroll
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
                                        // Binning scans every visible overlay sample (the whole
                                        // buffer when zoomed out); cache by view parameters so a
                                        // static viewport costs nothing per frame.
                                        let loop_start = tab.loop_region.map(|(a, _)| a).unwrap_or(0);
                                        let cache_key = crate::app::render::overlay_cache::OverlayBinsKey {
                                            overlay_revision: overlay.revision,
                                            buf_ptr: buf.as_ptr() as usize,
                                            buf_len: buf.len(),
                                            start,
                                            visible_len,
                                            startb,
                                            over_vis,
                                            bins,
                                            unwrap_base_total: if unwrap_preview { base_total } else { 0 },
                                            unwrap_loop_start: if unwrap_preview { loop_start } else { 0 },
                                            unwrap_overlay_total: if unwrap_preview { overlay_total } else { 0 },
                                        };
                                        let bins_values = self.overlay_bins_cache.get_or_compute(cache_key, || {
                                            if unwrap_preview {
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
                                            }
                                        });
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
                        let is_time_stretch = matches!(overlay.source_tool, ToolKind::TimeStretch | ToolKind::Speed);
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
                                && tab.tool_state.loop_apply_unroll
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
                                            egui::Stroke::new(1.8_f32, Color32::from_rgb(80, 240, 160))
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
                                        if let Some(lp) = last { painter.line_segment([lp, p], egui::Stroke::new(1.8_f32, Color32::from_rgb(80, 240, 160))); }
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
                                                painter.line_segment([egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))], egui::Stroke::new(1.6_f32, Color32::from_rgb(80, 240, 160)));
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
                                                painter.line_segment([egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))], egui::Stroke::new(1.6_f32, Color32::from_rgb(80, 240, 160)));
                                            }
                                        }
                                    }
                                } else {
                                    let overlay_sample_x = |overlay_sample: usize| {
                                        if overlay_total == base_total {
                                            geom.sample_center_x(overlay_sample)
                                        } else {
                                            let display_sample = ((overlay_sample as f64 + 0.5)
                                                * base_total as f64
                                                / overlay_total as f64)
                                                - 0.5;
                                            geom.fractional_sample_center_x_unclamped(display_sample)
                                                .clamp(geom.wave_left, geom.wave_left + geom.wave_w)
                                        }
                                    };
                                    let mut last: Option<egui::Pos2> = None;
                                    for i in startb..endb {
                                        let v = (buf[i] * scale).clamp(-1.0, 1.0);
                                        let sx = overlay_sample_x(i);
                                        let sy = Self::waveform_y_from_amp(
                                            lane_rect,
                                            tab.vertical_zoom,
                                            tab.vertical_view_center,
                                            v,
                                        );
                                        let p = egui::pos2(sx, sy);
                                        if let Some(lp) = last { painter.line_segment([lp, p], egui::Stroke::new(1.5_f32, Color32::from_rgb(80, 240, 160))); }
                                        last = Some(p);
                                    }
                                    if let Some(radius) = Self::waveform_sample_point_radius(spp) {
                                        for i in startb..endb {
                                            let v = (buf[i] * scale).clamp(-1.0, 1.0);
                                            let sx = overlay_sample_x(i);
                                            let sy = Self::waveform_y_from_amp(
                                                lane_rect,
                                                tab.vertical_zoom,
                                                tab.vertical_view_center,
                                                v,
                                            );
                                            painter.circle_filled(
                                                egui::pos2(sx, sy),
                                                radius,
                                                Color32::from_rgb(80, 240, 160),
                                            );
                                        }
                                        if overlay.source_tool == ToolKind::Pencil {
                                            if let Some(selection) = tab
                                                .pencil_draft
                                                .as_ref()
                                                .and_then(|draft| draft.selection.as_ref())
                                            {
                                                let applies_to_lane = use_mixdown
                                                    || channel_index.is_some_and(|channel| {
                                                        selection.channels.contains(&channel)
                                                    });
                                                if applies_to_lane {
                                                    let selected_start =
                                                        selection.start.max(startb);
                                                    let selected_end =
                                                        selection.end.min(endb);
                                                    for i in selected_start..selected_end {
                                                        let v = (buf[i] * scale)
                                                            .clamp(-1.0, 1.0);
                                                        let sx = overlay_sample_x(i);
                                                        let sy = Self::waveform_y_from_amp(
                                                            lane_rect,
                                                            tab.vertical_zoom,
                                                            tab.vertical_view_center,
                                                            v,
                                                        );
                                                        painter.circle_filled(
                                                            egui::pos2(sx, sy),
                                                            radius + 1.6,
                                                            Color32::from_rgb(255, 205, 80),
                                                        );
                                                        painter.circle_filled(
                                                            egui::pos2(sx, sy),
                                                            radius.max(1.1),
                                                            Color32::from_rgb(80, 240, 160),
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
                }
            }
            }

                // A lane nobody can hear recedes. Mute and solo reached the M/S
                // buttons and the engine but never the canvas, so a silenced
                // channel drew exactly as brightly as the one actually playing.
                // The scrim covers this lane's trace and its grid and nothing
                // above it, so the selection, markers and playhead stay equally
                // readable across every lane.
                if let Some(ch) = channel_index {
                    if !Self::editor_channel_is_audible(&*tab, ch) {
                        painter.rect_filled(lane_rect, 0.0, WAVE_INAUDIBLE_SCRIM);
                    }
                }
                }
            }

            // (Removed) global mono overlay to avoid double/triple drawing.

            // Overlay regions (loop/trim/fade) on top of waveform
            if tab.samples_len > 0 {
                let to_x = |samp: usize| geom.sample_boundary_x(samp);
                let marker_x = |samp: usize| geom.sample_center_x(samp);
                if let Some(overlay) = dynamics_overlay.as_ref() {
                    let first = tab.view_offset.min(tab.samples_len);
                    let last = (first + (wave_w * spp).ceil() as usize).min(tab.samples_len);
                    let area = egui::Rect::from_min_max(
                        egui::pos2(wave_left, rect.top()),
                        egui::pos2(wave_left + wave_w, rect.bottom()),
                    );
                    overlay.draw_gain(&painter, area, (first, last), to_x);
                }
                let draw_handle = |x: f32, col: Color32| {
                    let handle_w = 6.0;
                    let handle_h = 16.0;
                    let r = egui::Rect::from_min_max(
                        egui::pos2(x - handle_w * 0.5, rect.top()),
                        egui::pos2(x + handle_w * 0.5, rect.top() + handle_h),
                    );
                    painter.rect_filled(r, 2.0, col);
                };
                // Where the loop's own handles and labels live: under the
                // Time Stretch grip's band, so the two never overlap and each
                // glyph stands where its gesture actually starts.
                let loop_handle_top = rect.top() + SELECTION_STRETCH_HANDLE_H;
                let loop_label_top = loop_handle_top + LOOP_HANDLE_H + 1.0;
                let draw_loop_handle = |x: f32, col: Color32| {
                    painter.add(egui::Shape::convex_polygon(
                        vec![
                            egui::pos2(x - LOOP_HANDLE_W * 0.5, loop_handle_top),
                            egui::pos2(x + LOOP_HANDLE_W * 0.5, loop_handle_top),
                            egui::pos2(x, loop_handle_top + LOOP_HANDLE_H),
                        ],
                        col,
                        egui::Stroke::NONE,
                    ));
                };
                let sr = sr_ctx.max(1.0);

                let mut fade_in_handle: Option<f32> = None;
                let mut fade_out_handle: Option<f32> = None;

                // Extra selections (Ctrl+drag additive ranges) — dimmer blue
                {
                    let vis = (wave_w * spp).ceil() as usize;
                    let view_end = tab.view_offset.saturating_add(vis).min(tab.samples_len);
                    for &(a0, b0) in &tab.extra_selections {
                        let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                        if b < tab.view_offset || a > view_end { continue; }
                        let ax = to_x(a);
                        let bx = to_x(b);
                        let sel_rect = egui::Rect::from_min_max(
                            egui::pos2(ax, rect.top()),
                            egui::pos2(bx, rect.bottom()),
                        );
                        painter.rect_filled(sel_rect, 0.0, Color32::from_rgba_unmultiplied(70, 140, 255, 16));
                        painter.rect_stroke(
                            sel_rect,
                            0.0,
                            egui::Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(70, 140, 255, 100)),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
                // Primary selection overlay (tool-independent). In the
                // spectral views a frequency band clamps the rectangle
                // vertically inside each channel lane (time-frequency
                // selection); otherwise it spans the full height.
                if let Some((a0, b0)) = tab.selection {
                    let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                    if b >= tab.view_offset {
                        let vis = (wave_w * spp).ceil() as usize;
                        let end = tab.view_offset.saturating_add(vis).min(tab.samples_len);
                        if a <= end {
                            let ax = to_x(a);
                            let bx = to_x(b);
                            let fill = Color32::from_rgba_unmultiplied(70, 140, 255, 28);
                            let stroke = Color32::from_rgba_unmultiplied(70, 140, 255, 160);
                            let band = if spec_freq_view {
                                tab.freq_selection
                            } else {
                                None
                            };
                            let mut sel_rects: Vec<egui::Rect> = Vec::new();
                            match band {
                                None => {
                                    sel_rects.push(egui::Rect::from_min_max(
                                        egui::pos2(ax, rect.top()),
                                        egui::pos2(bx, rect.bottom()),
                                    ));
                                }
                                Some((lo_hz, hi_hz)) => {
                                    let denom = (spec_vis_max - spec_vis_min).max(f32::EPSILON);
                                    let frac_lo = ((Self::editor_spec_freq_to_frac(
                                        view_mode,
                                        lo_hz,
                                        spec_axis_max_freq,
                                        spec_mel_scale,
                                    ) - spec_vis_min)
                                        / denom)
                                        .clamp(0.0, 1.0);
                                    let frac_hi = ((Self::editor_spec_freq_to_frac(
                                        view_mode,
                                        hi_hz,
                                        spec_axis_max_freq,
                                        spec_mel_scale,
                                    ) - spec_vis_min)
                                        / denom)
                                        .clamp(0.0, 1.0);
                                    for lane in visible_lanes.clone() {
                                        let lane_bottom = lane_geom.lane_top(lane + 1);
                                        let y_top = lane_bottom - frac_hi * lane_h;
                                        let y_bottom = lane_bottom - frac_lo * lane_h;
                                        if y_bottom - y_top >= 0.5 {
                                            sel_rects.push(egui::Rect::from_min_max(
                                                egui::pos2(ax, y_top),
                                                egui::pos2(bx, y_bottom),
                                            ));
                                        }
                                    }
                                }
                            }
                            for sel_rect in sel_rects {
                                painter.rect_filled(sel_rect, 0.0, fill);
                                painter.rect_stroke(
                                    sel_rect,
                                    0.0,
                                    egui::Stroke::new(1.0_f32, stroke),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            // The edge line runs the full height and is a
                            // plain range resize anywhere along it. Only the
                            // tab at the very top stretches audio, and it is
                            // drawn only in the primary Waveform view — the
                            // blue fill above remains in every other view.
                            if view_mode == ViewMode::Waveform {
                                let handle_col = Color32::from_rgb(90, 220, 255);
                                // Dark enough to read against the grip's fill;
                                // the arrows are the only thing that says this
                                // tab does something the line below does not.
                                let arrow_col = Color32::from_rgb(12, 18, 24);
                                let draw_stretch_handle = |x: f32| {
                                    painter.line_segment(
                                        [
                                            egui::pos2(x, rect.top()),
                                            egui::pos2(x, rect.bottom()),
                                        ],
                                        egui::Stroke::new(2.0_f32, handle_col),
                                    );
                                    painter.rect_filled(
                                        egui::Rect::from_min_max(
                                            egui::pos2(
                                                x - SELECTION_STRETCH_HANDLE_W * 0.5,
                                                rect.top(),
                                            ),
                                            egui::pos2(
                                                x + SELECTION_STRETCH_HANDLE_W * 0.5,
                                                rect.top() + SELECTION_STRETCH_HANDLE_H,
                                            ),
                                        ),
                                        2.0,
                                        handle_col,
                                    );
                                    let cy = rect.top() + SELECTION_STRETCH_HANDLE_H * 0.5;
                                    for dir in [-1.0f32, 1.0f32] {
                                        painter.add(egui::Shape::line(
                                            vec![
                                                egui::pos2(x + dir * 1.0, cy - 2.5),
                                                egui::pos2(x + dir * 3.5, cy),
                                                egui::pos2(x + dir * 1.0, cy + 2.5),
                                            ],
                                            egui::Stroke::new(1.2_f32, arrow_col),
                                        ));
                                    }
                                };
                                if geom.contains_boundary(a) {
                                    draw_stretch_handle(ax);
                                }
                                if geom.contains_boundary(b) {
                                    draw_stretch_handle(bx);
                                }
                            }
                        }
                    }
                }

                // Harmonic-action overlay: translucent bands at k*f0 (+/-3%)
                // across the full width while the popup is open.
                if spec_freq_view {
                    if let Some(action) = self
                        .harmonic_action
                        .filter(|a| a.tab_id == tab.tab_id)
                    {
                        let denom = (spec_vis_max - spec_vis_min).max(f32::EPSILON);
                        let fill = Color32::from_rgba_unmultiplied(255, 176, 64, 36);
                        let stroke = Color32::from_rgba_unmultiplied(255, 176, 64, 140);
                        for k in 1..=action.harmonics.clamp(1, 16) {
                            let center = action.f0.max(1.0) * k as f32;
                            let lo_hz = center * 0.97;
                            let hi_hz = center * 1.03;
                            let frac_of = |hz: f32| {
                                ((Self::editor_spec_freq_to_frac(
                                    view_mode,
                                    hz,
                                    spec_axis_max_freq,
                                    spec_mel_scale,
                                ) - spec_vis_min)
                                    / denom)
                                    .clamp(0.0, 1.0)
                            };
                            let frac_lo = frac_of(lo_hz);
                            let frac_hi = frac_of(hi_hz);
                            if frac_hi <= 0.0 || frac_lo >= 1.0 {
                                continue;
                            }
                            for lane in visible_lanes.clone() {
                                let lane_bottom = lane_geom.lane_top(lane + 1);
                                let y_top = lane_bottom - frac_hi * lane_h;
                                let y_bottom = lane_bottom - frac_lo * lane_h;
                                if y_bottom - y_top < 0.5 {
                                    continue;
                                }
                                let band_rect = egui::Rect::from_min_max(
                                    egui::pos2(rect.left(), y_top),
                                    egui::pos2(rect.right(), y_bottom),
                                );
                                painter.rect_filled(band_rect, 0.0, fill);
                                painter.rect_stroke(
                                    band_rect,
                                    0.0,
                                    egui::Stroke::new(1.0_f32, stroke),
                                    egui::StrokeKind::Inside,
                                );
                            }
                        }
                    }
                }


                // Marker overlay: the file's markers, then (green) the beats
                // and sections Music Analyze shows but has not added.
                let provisional = &tab.music_analysis_draft.provisional_markers;
                if !tab.markers.is_empty() || !provisional.is_empty() {
                    let vis = (wave_w * spp).ceil() as usize;
                    let start = tab.view_offset.min(tab.samples_len);
                    let end = (start + vis).min(tab.samples_len);
                    let layers = [
                        (&tab.markers, Color32::from_rgb(255, 200, 80)),
                        (provisional, Color32::from_rgb(120, 220, 120)),
                    ];
                    for (markers, col) in layers {
                        for m in markers.iter() {
                            if m.sample < start || m.sample > end {
                                continue;
                            }
                            let x = marker_x(m.sample);
                            painter.line_segment(
                                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                                egui::Stroke::new(1.0_f32, col),
                            );
                        }
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
                            egui::Stroke::new(1.5_f32, line),
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
                            egui::Stroke::new(1.5_f32, line),
                        );
                        painter.line_segment(
                            [egui::pos2(bx, rect.top()), egui::pos2(bx, rect.bottom())],
                            egui::Stroke::new(1.5_f32, line),
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
                            egui::Stroke::new(2.0_f32, line),
                        );
                        draw_loop_handle(ax, line);
                        painter.text(
                            egui::pos2(ax + 8.0, loop_label_top),
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
                            egui::Stroke::new(2.0_f32, line),
                        );
                        painter.line_segment(
                            [egui::pos2(bx, rect.top()), egui::pos2(bx, rect.bottom())],
                            egui::Stroke::new(2.0_f32, line),
                        );
                        draw_loop_handle(ax, line);
                        draw_loop_handle(bx, line);
                        painter.text(
                            egui::pos2(ax + 8.0, loop_label_top),
                            egui::Align2::LEFT_TOP,
                            "S",
                            fid.clone(),
                            Color32::from_rgb(170, 200, 255),
                        );
                        painter.text(
                            egui::pos2(bx + 8.0, loop_label_top),
                            egui::Align2::LEFT_TOP,
                            "E",
                            fid.clone(),
                            Color32::from_rgb(170, 200, 255),
                        );
                        let dur = (b.saturating_sub(a)) as f32 / sr;
                        let label = crate::app::helpers::format_time_s(dur);
                        painter.text(
                            egui::pos2(ax + 8.0, loop_label_top + 14.0),
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
                                        egui::Stroke::new(2.0_f32, curve_col),
                                    );
                                }
                                if !uses_dip {
                                    if let Some(lp) = last_in_down {
                                        painter.line_segment(
                                            [lp, p_in_down],
                                            egui::Stroke::new(2.0_f32, curve_col),
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
                                        egui::Stroke::new(2.0_f32, curve_col),
                                    );
                                }
                                if !uses_dip {
                                    if let Some(lp) = last_out_up {
                                        painter.line_segment(
                                            [lp, p_out_up],
                                            egui::Stroke::new(2.0_f32, curve_col),
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
                            painter.line_segment([lp, p], egui::Stroke::new(2.0_f32, curve_col));
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
                            draw_handle(x1, Color32::from_rgb(80, 180, 255));
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
                            draw_handle(x0, Color32::from_rgb(255, 160, 90));
                            let fid = TextStyle::Monospace.resolve(ui.style());
                            let secs = (n_out as f32) / sr;
                            painter.text(
                                egui::pos2(x1 - 6.0, rect.bottom() - 18.0),
                                egui::Align2::RIGHT_BOTTOM,
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

                // Cursor feedback for editor handles. The per-tool handles
                // below are Waveform-only, but the selection's edges are
                // draggable in every view, so their branch reads `view_mode`
                // itself rather than sitting behind the gate.
                if pointer_over_waveform {
                    let handle_radius = 7.0;
                    let waveform_view = view_mode == ViewMode::Waveform;
                    if tab.selection_stretch_gesture.is_some() {
                        // A closed hand for the one gesture that rewrites audio.
                        hover_cursor = Some(egui::CursorIcon::Grabbing);
                    } else if tab.dragging_marker.is_some()
                        || tab.selection_edge_drag_anchor.is_some()
                    {
                        hover_cursor = Some(egui::CursorIcon::ResizeHorizontal);
                    } else if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                        let x = pos.x;
                        let near = |hx: f32| (x - hx).abs() <= handle_radius;
                        // Per-tool handles first, so the selection's own grip
                        // below can override them. A loop edge normally sits
                        // exactly on the selection edge, and the grip at the
                        // top belongs to the stretch even there.
                        match tab.active_tool {
                            ToolKind::LoopEdit if waveform_view => {
                                // The same reach, and the same "is it actually
                                // on screen" test, as the drag itself — so the
                                // cursor never offers a handle the press will
                                // not find. `to_x` clamps, and a resize arrow
                                // over the canvas border for a loop scrolled
                                // out of view is exactly the phantom the drag
                                // no longer answers.
                                if let Some((a, b)) = Self::normalized_loop_range(tab.loop_region) {
                                    let edge_near = |sample: usize| {
                                        geom.contains_boundary(sample)
                                            && (x - geom.sample_boundary_x_unclamped(sample)).abs()
                                                <= LOOP_EDGE_GRAB_RADIUS
                                    };
                                    if edge_near(a) || edge_near(b) {
                                        hover_cursor = Some(egui::CursorIcon::ResizeHorizontal);
                                    }
                                }
                            }
                            ToolKind::Fade if waveform_view => {
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
                        // The selection's edges are tool-independent. The grip
                        // at the top stretches audio (an open hand); the line
                        // below it only moves the range (a resize arrow).
                        if let Some((a0, b0)) = tab.selection {
                            let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                            if b > a {
                                let edge_near = |sample: usize| {
                                    geom.contains_boundary(sample)
                                        && near(geom.sample_boundary_x_unclamped(sample))
                                };
                                if edge_near(a) || edge_near(b) {
                                    // The grip exists only in the Waveform
                                    // view; elsewhere the whole line resizes.
                                    let on_grip = waveform_view
                                        && in_selection_stretch_band(rect.top(), pos.y);
                                    hover_cursor = Some(if on_grip {
                                        egui::CursorIcon::Grab
                                    } else {
                                        egui::CursorIcon::ResizeHorizontal
                                    });
                                }
                            }
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
                    painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(2.0_f32, Color32::from_rgb(70,140,255)));
                    // Playhead time label
                    let sr_f = sr_ctx.max(1.0);
                    let pos_time = (pos as f32) / sr_f;
                    let label = crate::app::helpers::format_time_s(pos_time);
                    let fid = TextStyle::Monospace.resolve(ui.style());
                    let text_pos = egui::pos2(x + 6.0, rect.top() + 2.0);
                    painter.text(text_pos, egui::Align2::LEFT_TOP, label, fid, Color32::from_rgb(180, 200, 220));
                }
            }

            // ---- Tool gesture overlays (drawn above waveform lanes) ----
            if view_mode == ViewMode::Waveform && display_samples_len > 0 {
                let overlay_font = TextStyle::Monospace.resolve(ui.style());
                let curve_vis = {
                    let vis_start = tab.view_offset.min(display_samples_len);
                    let vis_end = ((tab.view_offset as f64
                        + (wave_w as f64) * (geom.spp.max(0.0001) as f64))
                        .ceil() as usize)
                        .min(display_samples_len);
                    (vis_start, vis_end)
                };
                let curve_hover = ui.input(|i| i.pointer.hover_pos());
                let curve_sample_to_x =
                    |s: usize| geom.sample_center_x(s.min(display_samples_len));
                let curve_x_to_sample =
                    |x: f32| geom.x_to_display_sample(x.clamp(wave_left, wave_left + wave_w));
                // Gain automation curve (breakpoint polyline)
                if matches!(tab.active_tool, ToolKind::Gain) && tab.gain_env_enabled {
                    let curve_color = Color32::from_rgb(255, 170, 60);
                    let fallback_db = if tab.tool_state.gain_db.is_finite() {
                        tab.tool_state.gain_db
                    } else {
                        0.0
                    };
                    // Faint 0 dB reference line
                    let zero_y = frac_to_y(0.5);
                    painter.line_segment(
                        [
                            egui::pos2(wave_left, zero_y),
                            egui::pos2(wave_left + wave_w, zero_y),
                        ],
                        egui::Stroke::new(
                            1.0_f32,
                            Color32::from_rgba_unmultiplied(255, 170, 60, 50),
                        ),
                    );
                    let value_to_y = |db: f32| frac_to_y(0.5 + db / (GAIN_ENV_DB_RANGE * 2.0));
                    let y_to_value = |y: f32| (y_to_frac(y) - 0.5) * GAIN_ENV_DB_RANGE * 2.0;
                    let axis = CurveAxis {
                        value_to_y: &value_to_y,
                        y_to_value: &y_to_value,
                        sample_to_x: &curve_sample_to_x,
                        x_to_sample: &curve_x_to_sample,
                    };
                    let value_at = |sample: usize| {
                        crate::wave::gain_envelope_db_at(&tab.gain_env_points, fallback_db, sample)
                    };
                    curve_canvas::draw_breakpoint_curve(
                        &painter,
                        &axis,
                        &tab.gain_env_points,
                        &value_at,
                        curve_vis,
                        (wave_left, wave_left + wave_w),
                        &CurveLook {
                            color: curve_color,
                            width: 2.0,
                            editable: true,
                            label: &|db| format!("{db:+.1} dB"),
                            font: overlay_font.clone(),
                        },
                        tab.gain_env_drag,
                        curve_hover,
                    );
                }
                // EQ / Compressor / Limiter automation: every lane of the
                // active tool that is on; the one being edited bright, with
                // its points, the rest as faint lines.
                if matches!(
                    tab.active_tool,
                    ToolKind::Eq | ToolKind::Compressor | ToolKind::Limiter
                ) {
                    let st = tab.tool_state;
                    let selected = tab.automation_selected.filter(|p| p.tool() == tab.active_tool);
                    for lane in tab
                        .automation
                        .iter()
                        .filter(|l| l.param.tool() == tab.active_tool && l.enabled)
                    {
                        let param = lane.param;
                        let editing = selected == Some(param);
                        if lane.points.is_empty() && !editing {
                            continue;
                        }
                        let base = Self::automation_color(param);
                        let color = if editing && param.has_effect(&st) {
                            base
                        } else {
                            base.gamma_multiply(0.45)
                        };
                        let value_to_y = |v: f32| frac_to_y(param.to_frac(&st, v));
                        let y_to_value = |y: f32| param.from_frac(&st, y_to_frac(y));
                        let axis = CurveAxis {
                            value_to_y: &value_to_y,
                            y_to_value: &y_to_value,
                            sample_to_x: &curve_sample_to_x,
                            x_to_sample: &curve_x_to_sample,
                        };
                        let curve =
                            crate::automation::Curve::new(lane.points.clone(), param.scale());
                        let static_value = param.static_value(&st);
                        let value_at =
                            |sample: usize| curve.value_at(sample).unwrap_or(static_value);
                        curve_canvas::draw_breakpoint_curve(
                            &painter,
                            &axis,
                            &lane.points,
                            &value_at,
                            curve_vis,
                            (wave_left, wave_left + wave_w),
                            &CurveLook {
                                color,
                                width: if editing { 2.0 } else { 1.2 },
                                editable: editing,
                                label: &|v| param.format(v),
                                font: overlay_font.clone(),
                            },
                            if editing { tab.automation_drag } else { None },
                            curve_hover,
                        );
                        // Its name at the left edge, on the line.
                        let left_y = value_to_y(value_at(curve_vis.0));
                        let mut name = param.label();
                        if editing && lane.points.is_empty() {
                            name.push_str(" \u{2014} double-click the waveform to add a point");
                        }
                        painter.text(
                            egui::pos2(wave_left + 6.0, left_y - 3.0),
                            egui::Align2::LEFT_BOTTOM,
                            name,
                            overlay_font.clone(),
                            color,
                        );
                    }
                }
                // PitchShift: static line or breakpoint automation curve.
                if matches!(tab.active_tool, ToolKind::PitchShift) {
                    let pitch_color = Color32::from_rgb(220, 140, 255);
                    let mut semi = tab.tool_state.pitch_semitones;
                    if !semi.is_finite() {
                        semi = 0.0;
                    }
                    // Faint 0 st reference line
                    let zero_y = frac_to_y(0.5);
                    painter.line_segment(
                        [
                            egui::pos2(wave_left, zero_y),
                            egui::pos2(wave_left + wave_w, zero_y),
                        ],
                        egui::Stroke::new(
                            1.0_f32,
                            Color32::from_rgba_unmultiplied(220, 140, 255, 50),
                        ),
                    );
                    let semi_to_y = |value: f32| {
                        frac_to_y(0.5 + value / (PITCH_SEMI_RANGE * 2.0))
                    };
                    if tab.pitch_env_enabled {
                        let y_to_value = |y: f32| (y_to_frac(y) - 0.5) * PITCH_SEMI_RANGE * 2.0;
                        let axis = CurveAxis {
                            value_to_y: &semi_to_y,
                            y_to_value: &y_to_value,
                            sample_to_x: &curve_sample_to_x,
                            x_to_sample: &curve_x_to_sample,
                        };
                        let value_at = |sample: usize| {
                            crate::wave::pitch_envelope_semitones_at(
                                &tab.pitch_env_points,
                                semi,
                                sample,
                            )
                        };
                        curve_canvas::draw_breakpoint_curve(
                            &painter,
                            &axis,
                            &tab.pitch_env_points,
                            &value_at,
                            curve_vis,
                            (wave_left, wave_left + wave_w),
                            &CurveLook {
                                color: pitch_color,
                                width: 2.0,
                                editable: true,
                                label: &|st| format!("{st:+.2} st"),
                                font: overlay_font.clone(),
                            },
                            tab.pitch_env_drag,
                            curve_hover,
                        );
                    } else {
                        let y = semi_to_y(semi);
                        let width = if tab.pitch_drag_active { 2.5_f32 } else { 2.0_f32 };
                        painter.line_segment(
                            [
                                egui::pos2(wave_left, y),
                                egui::pos2(wave_left + wave_w, y),
                            ],
                            egui::Stroke::new(width, pitch_color),
                        );
                        painter.text(
                            egui::pos2(wave_left + wave_w - 6.0, y - 4.0),
                            egui::Align2::RIGHT_BOTTOM,
                            format!("{semi:+.2} st"),
                            overlay_font.clone(),
                            pitch_color,
                        );
                    }
                }
                // Both selection handles share one pitch-preserving Time
                // Stretch ghost, independent of whichever inspector tool is
                // selected. DSP starts only after the pointer is released.
                if let Some(gesture) = tab.selection_stretch_gesture {
                    let stretch_color = Color32::from_rgb(90, 220, 255);
                    let fixed_sample = match gesture.edge {
                        SelectionStretchEdge::Start => gesture.source_range.1,
                        SelectionStretchEdge::End => gesture.source_range.0,
                    };
                    let fixed_x = geom.sample_boundary_x_unclamped(fixed_sample);
                    let span_px = gesture.target_len as f32 / geom.spp.max(0.0001);
                    let target_x = match gesture.edge {
                        SelectionStretchEdge::Start => fixed_x - span_px,
                        SelectionStretchEdge::End => fixed_x + span_px,
                    };
                    let clipped_fixed_x = fixed_x.clamp(wave_left, wave_left + wave_w);
                    let clipped_target_x = target_x.clamp(wave_left, wave_left + wave_w);
                    painter.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(clipped_fixed_x.min(clipped_target_x), rect.top()),
                            egui::pos2(clipped_fixed_x.max(clipped_target_x), rect.bottom()),
                        ),
                        0.0,
                        Color32::from_rgba_unmultiplied(90, 220, 255, 34),
                    );
                    painter.line_segment(
                        [
                            egui::pos2(clipped_target_x, rect.top()),
                            egui::pos2(clipped_target_x, rect.bottom()),
                        ],
                        egui::Stroke::new(2.5_f32, stretch_color),
                    );
                    let source_len = gesture.source_range.1 - gesture.source_range.0;
                    let rate = selection_stretch_rate(source_len, gesture.target_len);
                    let dir_label = if rate > 1.0001 {
                        " (faster/shorter)"
                    } else if rate < 0.9999 {
                        " (slower/longer)"
                    } else {
                        ""
                    };
                    let (label_pos, align) = if clipped_target_x > wave_left + wave_w - 170.0 {
                        (
                            egui::pos2(clipped_target_x - 6.0, rect.top() + 16.0),
                            egui::Align2::RIGHT_TOP,
                        )
                    } else {
                        (
                            egui::pos2(clipped_target_x + 6.0, rect.top() + 16.0),
                            egui::Align2::LEFT_TOP,
                        )
                    };
                    painter.text(
                        label_pos,
                        align,
                        format!("x{rate:.2}{dir_label}"),
                        overlay_font.clone(),
                        stretch_color,
                    );
                }
            }

            // ---- De-click scan markers (Waveform view): red bands over the
            // detected click spans until the buffer or sensitivity changes.
            if view_mode == ViewMode::Waveform && display_samples_len > 0 {
                if let Some(scan) = &tab.declick_scan {
                    let fill = Color32::from_rgba_unmultiplied(255, 70, 70, 46);
                    let edge = Color32::from_rgba_unmultiplied(255, 90, 90, 150);
                    for &(s, e) in &scan.spans {
                        let x0 = geom.sample_center_x(s.min(display_samples_len));
                        let x1 = geom.sample_center_x(e.min(display_samples_len));
                        if x1 < wave_left - 4.0 || x0 > wave_left + wave_w + 4.0 {
                            continue;
                        }
                        let x1 = x1.max(x0 + 2.0);
                        painter.rect_filled(
                            egui::Rect::from_min_max(
                                egui::pos2(x0, rect.top()),
                                egui::pos2(x1, rect.bottom()),
                            ),
                            0.0,
                            fill,
                        );
                        painter.line_segment(
                            [egui::pos2(x0, rect.top()), egui::pos2(x0, rect.bottom())],
                            egui::Stroke::new(1.0_f32, edge),
                        );
                    }
                }
            }

            // ---- Spectral Warp arrows (Spec/Log views) ----
            if matches!(view_mode, ViewMode::Spectrogram | ViewMode::Log)
                && display_samples_len > 0
                && (tab.spectral_warp_edit || !tab.spectral_warp_points.is_empty())
            {
                let overlay_font = TextStyle::Monospace.resolve(ui.style());
                let warp_color = Color32::from_rgb(255, 130, 210);
                let halo = Color32::from_rgba_unmultiplied(0, 0, 0, 170);
                let spec_freq_to_y = |hz: f32, lane: usize| -> f32 {
                    let frac = Self::editor_spec_freq_to_frac(
                        view_mode,
                        hz,
                        spec_axis_max_freq,
                        spec_mel_scale,
                    );
                    let frac_local = ((frac - spec_vis_min)
                        / (spec_vis_max - spec_vis_min).max(1e-6))
                    .clamp(0.0, 1.0);
                    let lane_top = lane_geom.lane_top(lane);
                    lane_top + (1.0 - frac_local) * lane_h.max(1.0)
                };
                let hover = ui.input(|i| i.pointer.hover_pos());
                for lane in visible_lanes.clone() {
                    for (idx, p) in tab.spectral_warp_points.iter().enumerate() {
                        let x = geom.sample_center_x(p.sample.min(display_samples_len));
                        if x < wave_left - 20.0 || x > wave_left + wave_w + 20.0 {
                            continue;
                        }
                        let y0 = spec_freq_to_y(p.freq_hz, lane);
                        let y1 = spec_freq_to_y(p.freq_hz + p.delta_hz, lane);
                        painter.line_segment(
                            [egui::pos2(x, y0), egui::pos2(x, y1)],
                            egui::Stroke::new(3.5_f32, halo),
                        );
                        painter.line_segment(
                            [egui::pos2(x, y0), egui::pos2(x, y1)],
                            egui::Stroke::new(1.8_f32, warp_color),
                        );
                        painter.circle_stroke(
                            egui::pos2(x, y0),
                            3.5,
                            egui::Stroke::new(1.5_f32, warp_color),
                        );
                        let grabbed = tab.spectral_warp_drag == Some(idx);
                        let hovered = hover
                            .map(|h| {
                                h.distance(egui::pos2(x, y1)) <= 10.0
                                    || h.distance(egui::pos2(x, y0)) <= 10.0
                            })
                            .unwrap_or(false);
                        let r = if grabbed || hovered { 5.0 } else { 3.5 };
                        painter.circle_filled(egui::pos2(x, y1), r, warp_color);
                        if (y1 - y0).abs() > 8.0 {
                            let dir = (y1 - y0).signum();
                            painter.add(egui::Shape::line(
                                vec![
                                    egui::pos2(x - 4.0, y1 - dir * 6.0),
                                    egui::pos2(x, y1),
                                    egui::pos2(x + 4.0, y1 - dir * 6.0),
                                ],
                                egui::Stroke::new(1.8_f32, warp_color),
                            ));
                        }
                        if grabbed || hovered {
                            painter.text(
                                egui::pos2(x + 8.0, y1),
                                egui::Align2::LEFT_CENTER,
                                format!("{:+.0} Hz", p.delta_hz),
                                overlay_font.clone(),
                                warp_color,
                            );
                        }
                    }
                }
            }

            // ---- Spectral Brush stamps (Spec/Log views): translucent discs
            // marking painted attenuation, deeper cuts drawn more opaque.
            if matches!(view_mode, ViewMode::Spectrogram | ViewMode::Log)
                && display_samples_len > 0
                && !tab.spectral_brush_stamps.is_empty()
            {
                let spec_freq_to_y = |hz: f32, lane: usize| -> f32 {
                    let frac = Self::editor_spec_freq_to_frac(
                        view_mode,
                        hz,
                        spec_axis_max_freq,
                        spec_mel_scale,
                    );
                    let frac_local = ((frac - spec_vis_min)
                        / (spec_vis_max - spec_vis_min).max(1e-6))
                    .clamp(0.0, 1.0);
                    let lane_top = lane_geom.lane_top(lane);
                    lane_top + (1.0 - frac_local) * lane_h.max(1.0)
                };
                for lane in visible_lanes.clone() {
                    for s in tab.spectral_brush_stamps.iter() {
                        let x = geom.sample_center_x(s.sample.min(display_samples_len));
                        if x < wave_left - 20.0 || x > wave_left + wave_w + 20.0 {
                            continue;
                        }
                        let y = spec_freq_to_y(s.freq_hz, lane);
                        // Radius from the frequency sigma so the disc roughly
                        // matches the brush's audible footprint.
                        let y_edge = spec_freq_to_y(s.freq_hz + s.freq_sigma_hz, lane);
                        let r = (y - y_edge).abs().clamp(3.0, 60.0);
                        let alpha = (40.0 + (s.cut_db / 80.0) * 120.0).clamp(40.0, 160.0) as u8;
                        painter.circle_filled(
                            egui::pos2(x, y),
                            r,
                            Color32::from_rgba_unmultiplied(255, 80, 80, alpha / 3),
                        );
                        painter.circle_stroke(
                            egui::pos2(x, y),
                            r,
                            egui::Stroke::new(
                                1.0_f32,
                                Color32::from_rgba_unmultiplied(255, 110, 110, alpha),
                            ),
                        );
                    }
                }
            }

            // Which channel each lane is: once the lanes scroll, nothing else
            // tells lane 17 from lane 18. A mixdown is one lane and needs none.
            if !use_mixdown && lane_count > 1 {
                Self::draw_editor_lane_labels(
                    &painter,
                    lane_geom,
                    visible_lanes.clone(),
                    &visible_channels,
                    lane_speaker_layout.as_deref(),
                    wave_left,
                );
            }
            if let Some(bar_rect) = lane_scrollbar_rect {
                if let Some(next_scroll) =
                    Self::draw_editor_lane_scrollbar(ui, bar_rect, lane_geom)
                {
                    tab.lane_scroll_px = next_scroll;
                    ctx.request_repaint();
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

            if let Some(range_rect) = spectro_color_range_rect {
                let edit = Self::draw_editor_spectro_color_range(
                    ui,
                    range_rect,
                    &mut self.spectro_cfg,
                );
                if edit.changed {
                    ctx.request_repaint();
                }
                save_spectro_color_range |= edit.committed;
            }

            if display_samples_len > 0 {
                let spp = tab.samples_per_px.max(0.0001);
                let vis = (wave_w * spp).ceil() as usize;
                let max_left = display_samples_len.saturating_sub(vis);
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
                    display_samples_len,
                    sr_ctx.max(1.0) as u32,
                    tab.view_offset,
                    vis,
                    gutter_w,
                    wave_w,
                ) {
                    Self::editor_set_view_offset(tab, next_view);
                }
            }

            // The splitter and the mini meter, on the column's bottom edge.
            // Placed from the bottom rather than after the navigator, so the
            // meter stays put whatever the waveform above it does.
            if column_layout.shows_meter() {
                let meter_rect = egui::Rect::from_min_max(
                    egui::pos2(column_rect.left(), column_rect.bottom() - column_layout.meter_h),
                    column_rect.max,
                );
                let splitter_rect = egui::Rect::from_min_max(
                    egui::pos2(
                        column_rect.left(),
                        meter_rect.top() - lanes_layout::METER_SPLITTER_H,
                    ),
                    egui::pos2(column_rect.right(), meter_rect.top()),
                );
                tab.last_mini_meter_rect = Some(meter_rect);
                tab.last_meter_splitter_rect = Some(splitter_rect);
                let splitter_resp = ui
                    .allocate_rect(splitter_rect, egui::Sense::click_and_drag())
                    .on_hover_text("Drag to resize the meter. Double-click to reset it.");
                let splitter_hot = splitter_resp.hovered() || splitter_resp.dragged();
                if splitter_hot {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                }
                ui.painter().hline(
                    splitter_rect.x_range(),
                    splitter_rect.center().y,
                    egui::Stroke::new(
                        if splitter_hot { 2.0_f32 } else { 1.0_f32 },
                        if splitter_hot {
                            Color32::from_rgb(90, 170, 255)
                        } else {
                            ui.visuals().widgets.noninteractive.bg_stroke.color
                        },
                    ),
                );
                // The splitter also senses drags, so `double_clicked()` alone
                // never fires on it; see `helpers::note_repeated_click`.
                let splitter_reset = splitter_resp.double_clicked()
                    || (splitter_resp.clicked()
                        && splitter_resp.interact_pointer_pos().is_some_and(|pos| {
                            let click_id = mini_meter_height_id.with("last_click");
                            let mut last: Option<(std::time::Instant, egui::Pos2)> =
                                ctx.data_mut(|d| d.get_temp(click_id)).flatten();
                            let repeat =
                                crate::app::helpers::note_repeated_click(&mut last, pos);
                            ctx.data_mut(|d| d.insert_temp(click_id, last));
                            repeat
                        }));
                if splitter_reset {
                    ctx.data_mut(|d| {
                        d.insert_temp(mini_meter_height_id, lanes_layout::MINI_METER_DEFAULT_H)
                    });
                    ctx.request_repaint();
                } else if splitter_resp.dragged() {
                    let next_h = lanes_layout::meter_height_after_drag(
                        &column_layout,
                        splitter_resp.drag_delta().y,
                    );
                    ctx.data_mut(|d| d.insert_temp(mini_meter_height_id, next_h));
                    ctx.request_repaint();
                }
                // The strip's frame is drawn even with nothing to meter yet
                // (a file still loading), so nothing moves when it arrives.
                ui.painter().rect_filled(meter_rect, 6.0, Color32::from_rgb(11, 12, 16));
                ui.painter().rect_stroke(
                    meter_rect,
                    6.0,
                    Stroke::new(1.0_f32, Color32::from_rgb(40, 46, 56)),
                    egui::StrokeKind::Inside,
                );
                if display_samples_len > 0 && !tab.loading {
                    let playing = self
                        .audio
                        .shared
                        .playing
                        .load(std::sync::atomic::Ordering::Relaxed);
                    let preview_audio = tab
                        .preview_audio_tool
                        .and_then(|tool| {
                            tab.preview_overlay
                                .as_ref()
                                .filter(|overlay| overlay.source_tool == tool)
                                .and_then(|_| {
                                    tab.preview_audio_buffer.as_ref().and_then(
                                        |(buffer_tool, buffer)| {
                                            (*buffer_tool == tool).then(|| buffer.clone())
                                        },
                                    )
                                })
                        })
                        .or_else(|| {
                            (tab.preview_audio_tool.is_some()
                                && playback_source
                                    == crate::app::PlaybackSourceKind::ToolPreview)
                                .then(|| self.audio.shared.samples.load_full())
                                .flatten()
                        });
                    // The picture must sit on the *drawn playhead*, not on a
                    // separately-derived clock, or the two disagree on screen
                    // by however much their derivations differ. The tab's
                    // display timeline is the source file's timeline for a
                    // video (they are read-only, so no edit can shift one
                    // against the other — see `EditorTab::read_only`), so
                    // converting the playhead to seconds is the whole of it.
                    let video_secs = playhead_display_now as f64
                        / Self::editor_display_sample_rate_for_tab(tab, out_sr).max(1) as f64;
                    // The object bed's newest frames end at the playhead.
                    let (meter_audio, meter_pos) = match (preview_audio.as_deref(), object_bed_meter.as_deref()) {
                        (Some(audio), _) => (Some(audio), pos_audio_now),
                        (None, Some(bed)) => (Some(bed), bed.len().saturating_sub(1)),
                        (None, None) => (None, pos_audio_now),
                    };
                    ui.scope_builder(egui::UiBuilder::new().max_rect(meter_rect), |ui| {
                        Self::draw_editor_mini_meter(
                            ui,
                            tab,
                            column_layout.meter_h,
                            playhead_display_now,
                            meter_audio,
                            meter_pos,
                            out_sr,
                            playing,
                            video_secs,
                            mini_meter_spectrum_interval_secs,
                            mini_meter_layout.as_deref(),
                            hrtf_active_path.as_deref() == Some(tab.path.as_path()),
                        );
                    });
                    video_request = Some((video_secs, playing));
                }
            } else {
                tab.last_mini_meter_rect = None;
                tab.last_meter_splitter_rect = None;
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

                // Divider: drag to resize the inspector.
                if !stacked_editor {
                    let (divider_rect, divider_resp) = ui.allocate_exact_size(
                        egui::vec2(6.0, inspector_area_h),
                        egui::Sense::drag(),
                    );
                    let hovered = divider_resp.hovered() || divider_resp.dragged();
                    if hovered {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }
                    ui.painter().vline(
                        divider_rect.center().x,
                        divider_rect.y_range(),
                        egui::Stroke::new(
                            if hovered { 2.0_f32 } else { 1.0_f32 },
                            if hovered {
                                Color32::from_rgb(90, 170, 255)
                            } else {
                                ui.visuals().widgets.noninteractive.bg_stroke.color
                            },
                        ),
                    );
                    if divider_resp.dragged() {
                        let delta = divider_resp.drag_delta().x;
                        ui.ctx().data_mut(|d| {
                            let w: &mut f32 = d.get_temp_mut_or(inspector_width_id, 310.0f32);
                            *w = (*w - delta).clamp(min_inspector_w, max_inspector_w);
                        });
                        ui.ctx().request_repaint();
                    }
                }
                // Inspector area (right)
                {
                    // A curve point, the pitch line, a fade handle or a
                    // spectral stroke the canvas moved is a change to the
                    // tool's settings like one made in the Inspector.
                    let pointer_down = ui.input(|i| i.pointer.primary_down());
                    self.note_canvas_draft_change(tab_idx, &canvas_draft_before, pointer_down);
                    self.ui_editor_inspector(
                        ui,
                        tab_idx,
                        &crate::app::ui::inspector::InspectorFrame {
                            width: inspector_w,
                            height: inspector_area_h,
                            scroll_source: editor_scroll_source,
                        },
                    );
                }
                if let Some((rate, range, viewport_restore)) = pending_selection_stretch_apply {
                    self.spawn_editor_selection_stretch_apply(
                        tab_idx,
                        rate,
                        range,
                        viewport_restore,
                    );
                }
                if pending_spectral_warp_preview {
                    self.spawn_spectral_warp_preview_for_tab(tab_idx);
                }
                if pending_pencil_preview_refresh {
                    self.editor_pencil_refresh_preview_audio(tab_idx);
                }
                if pending_spectral_brush_preview {
                    self.spawn_spectral_brush_preview_for_tab(tab_idx);
                }
                if let Some((samp, hz)) = pending_harmonic_click {
                    self.editor_harmonic_click(tab_idx, samp, hz);
                }
                if let Some(tab_id) = pending_scrub_begin {
                    self.scrub_begin(tab_id);
                }
                if let Some(center) = pending_scrub_update {
                    self.scrub_update(center);
                }
                if pending_scrub_end {
                    self.scrub_end();
                }
                if stop_playback {
                    self.audio.stop();
                }
                if need_restore_preview {
                    self.clear_preview_if_any(tab_idx);
                }
                if request_preview_refresh {
                    self.rebuild_tool_preview_for_tab(tab_idx);
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
                        let clamped_seek = seek_geom.clamp_display_sample(seek_display);
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
            };
            if stacked_editor {
                ui.vertical(|ui| {
                    draw_editor_body(ui);
                });
            } else {
                ui.horizontal_top(|ui| {
                    draw_editor_body(ui);
                });
            }
        } // end editor split
        if touch_spectro_cache {
            self.touch_spectro_cache(&spec_path);
        }
        let popout_requested = self
            .tabs
            .get_mut(tab_idx)
            .and_then(|tab| tab.video_panel.as_mut())
            .is_some_and(|panel| std::mem::take(&mut panel.popout_requested));
        if popout_requested {
            let tab_id = self.tabs[tab_idx].tab_id;
            self.open_detached_video_for_tab(tab_id, ctx);
        }
        if let Some((secs, playing)) = video_request {
            self.request_video_frame_for_tab(tab_idx, secs, playing);
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


        // perform pending actions after borrows end
        // The buttons go through the same dispatcher as Ctrl+Z / Ctrl+Y.
        // The canvas's right-click menu, applied now that the tab borrow is
        // gone. Opening the window puts the token where it can be typed after:
        // `insert_comment_reference` writes the window composer's draft, which
        // is a different buffer from the list row popup's.
        if let Some((path, anchor)) = pending_comment_ref {
            let reference = self.comment_ref_for_path(&path, anchor);
            self.insert_comment_reference(&reference);
            self.open_comments_window_for_path(&path);
        }
        if let Some(state) = pending_edit_undo.take() {
            self.push_editor_undo_state(tab_idx, state, true);
        }
        if apply_pending_loop {
            if let Some(tab_ro) = self.tabs.get(tab_idx) {
                self.apply_loop_mode_for_tab(tab_ro);
            }
        }
        if let Some(sample) = zoom_around_loop_point {
            // The same 0.9 step the wheel and `+` use, so one double click is
            // one recognisable notch rather than a jump to some fixed depth.
            self.editor_zoom_centered_on_sample(tab_idx, sample, 0.9);
        }
        if save_spectro_color_range {
            self.save_prefs();
        }
        self.editor_settle_annotation_if_idle(tab_idx, ctx);
        self.ui_editor_zoo_overlay(ctx, Some(tab_idx), editor_panel_rect);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        in_selection_stretch_band, nearest_handle, selection_stretch_rate,
        selection_stretch_target_len, waveform_render_lod, EditorDisplayGeometry,
        WaveformRenderLod, SELECTION_EDGE_GRAB_RADIUS, SELECTION_STRETCH_HANDLE_H,
        SELECTION_STRETCH_HANDLE_W,
    };
    use crate::app::types::{EditorHorizontalScrollSpeed, SelectionStretchEdge};

    /// DC at +0.5 with a single sign change `offset` frames after `cur`.
    fn dc_with_crossing_after(cur: usize, offset: usize) -> Vec<Vec<f32>> {
        let len = cur + offset + 16;
        vec![(0..len)
            .map(|i| if i < cur + offset { 0.5 } else { -0.5 })
            .collect()]
    }

    #[test]
    fn zero_cross_snap_reaches_one_second_at_any_rate() {
        // 1.5x the 48 kHz window: out of reach at 48 kHz, inside one
        // second at 96 kHz. A window counted in frames would miss it.
        let cur = 1_000;
        let offset = 72_000;
        let ch = dc_with_crossing_after(cur, offset);
        assert_eq!(super::zc_snap_nearest(&ch, 48_000, 0.0, cur), cur);
        assert_eq!(super::zc_snap_nearest(&ch, 96_000, 0.0, cur), cur + offset);
    }

    fn geom(view_offset: usize, spp: f32) -> EditorDisplayGeometry {
        EditorDisplayGeometry {
            wave_left: 100.0,
            wave_w: 800.0,
            spp,
            view_offset,
            view_offset_exact: view_offset as f64,
            display_samples_len: 100_000,
            visible_count: (800.0 * spp) as usize,
        }
    }

    #[test]
    fn waveform_lod_changes_without_an_early_pyramid_jump() {
        let base = 64.0;
        assert_eq!(waveform_render_lod(1.5, base), WaveformRenderLod::Raw);
        for spp in [2.0, 5.0, 31.5, 32.0, 63.5] {
            assert_eq!(
                waveform_render_lod(spp, base),
                WaveformRenderLod::VisibleMinMax,
                "spp={spp}"
            );
        }
        for spp in [64.0, 128.0] {
            assert_eq!(
                waveform_render_lod(spp, base),
                WaveformRenderLod::Pyramid,
                "spp={spp}"
            );
        }
        assert_eq!(
            waveform_render_lod(48.0, base),
            WaveformRenderLod::VisibleMinMax,
            "HiDPI density must be measured per device column"
        );
    }

    #[test]
    fn nearest_handle_ignores_anything_outside_the_radius() {
        let candidates = [(200.0f32, 7usize), (400.0, 9)];
        assert_eq!(nearest_handle(&candidates, 300.0, 7.0), None);
        assert_eq!(nearest_handle(&[], 200.0, 7.0), None);
    }

    #[test]
    fn nearest_handle_picks_the_closer_one() {
        let candidates = [(200.0f32, 7usize), (208.0, 9)];
        assert_eq!(nearest_handle(&candidates, 201.0, 7.0), Some(7));
        assert_eq!(nearest_handle(&candidates, 207.0, 7.0), Some(9));
    }

    /// On a selection narrower than two grab radii both handles are in range.
    /// The tie goes to the later candidate — the end — so the range extends
    /// outward, which is the adjustment the feature exists for. The loop
    /// marker code's `if / else if` would always take the start instead.
    #[test]
    fn nearest_handle_breaks_ties_toward_the_end() {
        let candidates = [(200.0f32, 7usize), (200.0, 9)];
        assert_eq!(nearest_handle(&candidates, 200.0, 7.0), Some(9));
    }

    /// The clamped `sample_boundary_x` reports `wave_left` for a boundary
    /// scrolled off to the left, which would hit-test as a phantom handle
    /// pinned to the canvas edge.
    #[test]
    fn offscreen_boundaries_are_excluded_rather_than_clamped_onto_the_edge() {
        let g = geom(5_000, 1.0);
        assert!(!g.contains_boundary(0));
        assert_eq!(g.sample_boundary_x(0), g.wave_left);
        assert!(g.sample_boundary_x_unclamped(0) < g.wave_left);

        assert!(!g.contains_boundary(99_000));
        assert!(g.sample_boundary_x_unclamped(99_000) > g.wave_left + g.wave_w);
    }

    #[test]
    fn onscreen_boundaries_agree_with_the_clamped_helper() {
        let g = geom(5_000, 1.0);
        for sample in [5_000usize, 5_400, 5_799] {
            assert!(g.contains_boundary(sample), "sample={sample}");
            let a = g.sample_boundary_x_unclamped(sample);
            let b = g.sample_boundary_x(sample);
            assert!((a - b).abs() < 0.001, "sample={sample}: {a} vs {b}");
        }
    }

    /// Staying strictly under the 8 px playhead snap means grabbing a handle
    /// can never, on the first frame, teleport the edge onto the playhead.
    #[test]
    fn the_grab_radius_stays_under_the_playhead_snap() {
        assert!(SELECTION_EDGE_GRAB_RADIUS < 8.0);
        assert!(SELECTION_EDGE_GRAB_RADIUS > 0.0);
    }

    #[test]
    fn selection_stretch_drag_is_symmetric_for_both_edges() {
        let source_len = 400usize;
        let end_longer =
            selection_stretch_target_len(source_len, SelectionStretchEdge::End, 75.0, 2.0);
        let start_longer =
            selection_stretch_target_len(source_len, SelectionStretchEdge::Start, -75.0, 2.0);
        assert_eq!(end_longer, 550);
        assert_eq!(start_longer, end_longer);

        let end_shorter =
            selection_stretch_target_len(source_len, SelectionStretchEdge::End, -75.0, 2.0);
        let start_shorter =
            selection_stretch_target_len(source_len, SelectionStretchEdge::Start, 75.0, 2.0);
        assert_eq!(end_shorter, 250);
        assert_eq!(start_shorter, end_shorter);
    }

    #[test]
    fn selection_stretch_target_and_rate_clamp_to_quarter_and_four_x() {
        let source_len = 400usize;
        let shortest =
            selection_stretch_target_len(source_len, SelectionStretchEdge::End, -10_000.0, 10.0);
        let longest =
            selection_stretch_target_len(source_len, SelectionStretchEdge::End, 10_000.0, 10.0);
        assert_eq!(shortest, 100);
        assert_eq!(longest, 1_600);
        assert_eq!(selection_stretch_rate(source_len, shortest), 4.0);
        assert_eq!(selection_stretch_rate(source_len, longest), 0.25);
    }

    /// The whole contract of the split: what is drawn as the grip is what
    /// stretches, and one pixel below it is a different, harmless gesture.
    #[test]
    fn the_stretch_band_is_exactly_the_drawn_grip() {
        let top = 100.0;
        assert!(in_selection_stretch_band(top, top));
        assert!(in_selection_stretch_band(
            top,
            top + SELECTION_STRETCH_HANDLE_H
        ));
        assert!(!in_selection_stretch_band(
            top,
            top + SELECTION_STRETCH_HANDLE_H + 0.1
        ));
        assert!(!in_selection_stretch_band(top, top - 0.1));
    }

    /// The grab radius must cover the tab the user is aiming at, or part of
    /// what is painted as the grip would quietly resize instead.
    #[test]
    fn everything_drawn_in_the_grip_is_grabbable() {
        assert!(SELECTION_EDGE_GRAB_RADIUS >= SELECTION_STRETCH_HANDLE_W * 0.5);
    }

    /// The destructive gesture stays the small target. 180.0 is the canvas's
    /// minimum height, so the grip can never grow into most of the edge line.
    #[test]
    fn the_grip_leaves_the_edge_line_to_the_resize() {
        assert!(SELECTION_STRETCH_HANDLE_H < 180.0 * 0.25);
    }

    #[test]
    fn one_wheel_notch_pans_the_same_share_of_the_viewport_at_any_width() {
        // The step this replaced was a flat 60 px, so a wide window crossed
        // proportionally less of the file per notch than a narrow one, and a
        // long take took the same many notches however far it was zoomed out.
        let notch = 50.0f32;
        for width in [320.0f32, 900.0, 2400.0] {
            let moved = notch
                * crate::app::WavesPreviewer::editor_horizontal_pan_gain(
                    width,
                    EditorHorizontalScrollSpeed::X1,
                );
            assert!(
                (moved / width - 0.15).abs() < 1.0e-4,
                "a notch should move 15% of a {width}px viewport, moved {moved}"
            );
        }
    }

    #[test]
    fn the_speed_setting_multiplies_that_share() {
        let width = 900.0f32;
        let base = crate::app::WavesPreviewer::editor_horizontal_pan_gain(
            width,
            EditorHorizontalScrollSpeed::X1,
        );
        for (speed, factor) in [
            (EditorHorizontalScrollSpeed::X2, 2.0f32),
            (EditorHorizontalScrollSpeed::X4, 4.0),
        ] {
            let scaled = crate::app::WavesPreviewer::editor_horizontal_pan_gain(width, speed);
            assert!(
                (scaled / base - factor).abs() < 1.0e-4,
                "{speed:?} should be {factor}x"
            );
        }
    }

    #[test]
    fn a_degenerate_canvas_width_still_yields_a_finite_step() {
        for width in [0.0f32, -5.0] {
            let gain = crate::app::WavesPreviewer::editor_horizontal_pan_gain(
                width,
                EditorHorizontalScrollSpeed::X1,
            );
            assert!(gain.is_finite() && gain > 0.0, "width {width} gave {gain}");
        }
    }
}
