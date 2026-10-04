//! The Multi Edits workspace: the list pane on the left, the timeline on the
//! right. What each part does is in `docs/MULTI_EDITS_SPEC.md`; the picture
//! windows of video tracks are `multi_edit_video.rs`.
//!
//! The timeline is drawn from a copy of the document taken at the top of the
//! frame; everything the user does while it is drawn is collected as an
//! [`Action`] and applied afterwards. Drawing never holds the document while
//! editing it, and one frame's edits land together.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};

use crate::app::input_focus::UiSurface;
use crate::app::multi_edit::{
    clamp_scroll_secs, grid_points, grid_step_secs, snap_secs, snap_span, zoom_out_limit,
    AutomationLane,
    Clip, LaneParam, MultiEditDoc, Track, TrackKind, TrackOutput, MAX_LANE_HEIGHT, MAX_PX_PER_SEC,
    MAX_TRACK_HEIGHT, MAX_TRACK_ZOOM, MIN_LANE_HEIGHT, MIN_TRACK_HEIGHT, MIN_TRACK_ZOOM,
    TRACK_GAIN_MAX_DB, TRACK_GAIN_MIN_DB,
};
use crate::app::multi_edit_ops::{
    ClipDrag, ClipDragKind, DropPreview, LaneDrag, Marquee, PointEditor, RowResize, SourceSlot,
    MULTI_EDIT_GRID_MIN_PX,
};
use crate::app::multi_edit_render::{
    covered_spans, crossfade_segments, draw_order, xfade_gain, XfadeSeg, XfadeShape,
};
use crate::app::channel_layout_ops::layout_name;
use crate::app::types::{ColumnId, ListViewProfile};
use crate::app::WavesPreviewer;
use crate::audio_channels::{Layout, SpeakerPos, PRESETS};

/// Rows dragged from the list pane onto the timeline.
pub(crate) struct MultiEditRowDrag(pub Vec<PathBuf>);

/// Width of the track header column (name, fader, M/S).
const HEADER_W: f32 = 176.0;
/// Inner padding of a track or lane header.
const HEADER_PAD: f32 = 6.0;
/// One line of header controls.
const HEADER_LINE_H: f32 = 20.0;
/// Side of a square header button (M, S, the lane fold arrow).
const HEADER_BUTTON: f32 = 22.0;
/// Width of a video track's picture-window toggle.
const VIDEO_BUTTON_W: f32 = 46.0;
/// Width of a track's output chip ("St", "L", "LFE", "Ch 13").
const OUTPUT_CHIP_W: f32 = 34.0;
/// Ruler height: tick labels below, marker flags above.
const RULER_H: f32 = 30.0;
/// The row under the tracks that holds the add button and takes drops that
/// should make a new track.
const ADD_ROW_H: f32 = 64.0;
/// How far inside a clip's edge a press trims instead of moving.
const EDGE_GRAB_PX: f32 = 6.0;
/// Side of the square handle at a clip's top corner that sets its fade.
const FADE_HANDLE_PX: f32 = 9.0;
/// How close, on screen, a dragged thing must come to a snap target.
const SNAP_PX: f32 = 8.0;
/// How far an automation lane is indented under its track, as in the design.
const LANE_INDENT: f32 = 14.0;
const POINT_RADIUS: f32 = 5.0;
/// A lane's value label is this tall; labels closer than this are dropped.
const LANE_LABEL_H: f32 = 11.0;
const POINT_RADIUS_HOT: f32 = 7.0;
/// How close a press must land to a point to grab it rather than add one.
const POINT_GRAB_PX: f32 = 9.0;
const LANE_STROKE: f32 = 2.0;
const CLIP_CORNER: f32 = 6.0;
/// Height of the strip at a header's bottom edge that resizes its row.
const RESIZE_GRAB_PX: f32 = 5.0;
/// Horizontal zoom per button press.
const ZOOM_BUTTON_STEP: f32 = 1.5;
/// Vertical zoom per button press.
const VERTICAL_ZOOM_STEP: f32 = 1.25;
/// Marker flags: the triangle's size, and the band of the ruler they use.
const MARKER_FLAG: f32 = 7.0;

const AUDIO_CLIP_FILL: Color32 = Color32::from_rgb(46, 92, 140);
const VIDEO_CLIP_FILL: Color32 = Color32::from_rgb(104, 70, 150);
const MISSING_CLIP_FILL: Color32 = Color32::from_rgb(110, 50, 50);
const WAVE_COLOR: Color32 = Color32::from_rgb(190, 215, 240);
/// The waveform of a clip where it lies over another: warm, so the two
/// waveforms in a crossfade are told apart.
const COVER_WAVE_COLOR: Color32 = Color32::from_rgb(250, 205, 150);
/// How much of a clip's fill is left where it lies over another clip, so
/// the one under it shows through.
const COVER_FILL_ALPHA: f32 = 0.3;
/// A crossfade's curves.
const XFADE_COLOR: Color32 = Color32::from_rgb(255, 196, 110);
/// The cut tool's line.
const CUT_LINE_COLOR: Color32 = Color32::from_rgb(255, 120, 90);
const PLAYHEAD_COLOR: Color32 = Color32::from_rgb(235, 80, 60);
const MARKER_COLOR: Color32 = Color32::from_rgb(120, 200, 255);

/// Each parameter's colour, in its lane and over folded clips.
fn param_color(param: LaneParam) -> Color32 {
    match param {
        LaneParam::Gain => Color32::from_rgb(240, 190, 90),
        LaneParam::Pitch => Color32::from_rgb(120, 220, 210),
        LaneParam::Pan => Color32::from_rgb(150, 220, 120),
        LaneParam::Mute => Color32::from_rgb(235, 120, 120),
    }
}

/// The value lines a lane draws, with their labels.
fn lane_ticks(param: LaneParam) -> Vec<(f32, String)> {
    let label = |v: f32| param.format_value(v);
    match param {
        LaneParam::Gain => [12.0, 0.0, -12.0, -24.0, -36.0, -48.0]
            .into_iter()
            .map(|v| (v, format!("{v:+.0}")))
            .collect(),
        LaneParam::Pitch => [24.0, 12.0, 0.0, -12.0, -24.0]
            .into_iter()
            .map(|v| (v, format!("{v:+.0}")))
            .collect(),
        LaneParam::Pan => vec![(1.0, label(1.0)), (0.0, label(0.0)), (-1.0, label(-1.0))],
        LaneParam::Mute => vec![(1.0, label(1.0)), (0.0, label(0.0))],
    }
}

/// Seconds <-> screen x for the visible part of the timeline.
#[derive(Clone, Copy)]
struct TimeMap {
    left: f32,
    right: f32,
    px_per_sec: f32,
    scroll_secs: f64,
}

impl TimeMap {
    fn x(&self, secs: f64) -> f32 {
        self.left + ((secs - self.scroll_secs) * self.px_per_sec as f64) as f32
    }

    fn secs(&self, x: f32) -> f64 {
        (self.scroll_secs + ((x - self.left) / self.px_per_sec) as f64).max(0.0)
    }

    fn visible_secs(&self) -> (f64, f64) {
        (self.secs(self.left), self.secs(self.right))
    }

    fn width(&self) -> f32 {
        (self.right - self.left).max(1.0)
    }
}

/// What snapping may catch this frame.
struct Snap {
    candidates: Vec<f64>,
    step: f64,
    threshold: f64,
    off: bool,
}

impl Snap {
    /// The candidates near `times`: every edge, marker and the playhead
    /// but those in `except`, and the grid lines either side of each time.
    fn candidates_near(&self, times: &[f64], except: &[f64]) -> Vec<f64> {
        let mut candidates: Vec<f64> = self
            .candidates
            .iter()
            .copied()
            .filter(|c| !except.iter().any(|e| (e - c).abs() < 1e-9))
            .collect();
        if self.step > 0.0 {
            for t in times {
                candidates.push((t / self.step).floor() * self.step);
                candidates.push((t / self.step).ceil() * self.step);
            }
        }
        candidates
    }

    /// `t` caught by the nearest clip edge, marker, the playhead or grid line.
    fn apply(&self, t: f64, except: &[f64]) -> f64 {
        if self.off {
            return t;
        }
        snap_secs(t, &self.candidates_near(&[t], except), self.threshold)
    }

    /// The start of a span `len` long, caught by whichever end is nearer.
    fn apply_span(&self, start: f64, len: f64, except: &[f64]) -> f64 {
        if self.off {
            return start;
        }
        let candidates = self.candidates_near(&[start, start + len], except);
        snap_span(start, len, &candidates, self.threshold)
    }
}

/// Everything a frame of the timeline asked for, applied after drawing.
enum Action {
    SelectClip(Option<String>),
    SelectTrack(Option<String>),
    BeginClipDrag(ClipDrag),
    ClipCommand(String, ClipCommand),
    DeleteSelected,
    Seek(f64),
    DropRows {
        track: Option<usize>,
        at_secs: f64,
        paths: Vec<PathBuf>,
    },
    Checkpoint,
    SetVolume(usize, f32),
    ToggleMute(usize),
    ToggleSolo(usize),
    ToggleLanes(usize),
    ToggleVideo(String, bool),
    BeginRenameTrack(usize),
    CommitRenameTrack(usize, String),
    CancelRenameTrack,
    AddLane(usize, LaneParam),
    RemoveLane(usize, String),
    AddTrack(TrackKind),
    RemoveTrack(usize),
    InsertPoint {
        track: usize,
        lane: String,
        secs: f64,
        value: f32,
    },
    BeginPointDrag(LaneDrag),
    OpenPointEditor(PointEditor),
    BeginResize(RowResize),
    AddMarker,
    CopyClip(String),
    CutClip(String),
    /// Split a clip at a time (the cut tool, or Alt+click).
    SplitAt(String, f64),
    /// Split a clip into one track per channel of its source.
    SplitChannels(String),
    /// Send a track (by id) to the front pair, or to one channel in mono.
    SetTrackOutput(String, TrackOutput),
    SetCutTool(bool),
    /// Select the track this many rows down (up when negative).
    StepTrack(i32),
    /// Ctrl+click: in or out of the selection.
    ToggleClip(String),
    /// Shift+click: into the selection.
    AddClip(String),
    SelectAllClips,
    BeginMarquee {
        origin: Pos2,
        additive: bool,
    },
    /// Paste at the playhead, on this track when it takes the clip.
    Paste(Option<String>),
    BeginMarkerDrag(String),
    BeginRenameMarker(String),
    RemoveMarker(String),
    Zoom {
        factor: f32,
        anchor_secs: f64,
        anchor_x: f32,
    },
    ZoomVertical(f32),
    Scroll(f64),
    /// The wheel: move the rows by this much.
    ScrollYBy(f32),
    /// The scroll bar: the rows are now here.
    ScrollYTo(f32),
}

/// What an inline name field did this frame.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NameEdit {
    Editing,
    Commit,
    Cancel,
}

/// An inline name field (a timeline's, a track's, a marker's).
///
/// It takes the keyboard once, when it opens, and never again: asked every
/// frame, it pulled the caret back after every click elsewhere and could not
/// be left. It ends on Enter, on a click anywhere outside it, or when the
/// keyboard goes elsewhere; Escape cancels.
///
/// While an input method is converting, Enter and Escape belong to the
/// conversion. Windows' egui-winit does not deliver them at all; another
/// integration might, so the field takes Enter itself (`return_key(None)`
/// -- egui's own handling would drop the focus mid-conversion) and ignores
/// both keys until the conversion is over. A click elsewhere still leaves.
fn inline_name_field(
    ui: &mut egui::Ui,
    rect: Option<Rect>,
    text: &mut String,
    width: f32,
    focus_now: bool,
    ime_busy: bool,
) -> NameEdit {
    let edit = egui::TextEdit::singleline(text)
        .desired_width(width)
        .return_key(None);
    let resp = match rect {
        Some(rect) => ui.put(rect, edit),
        None => ui.add(edit),
    };
    if focus_now {
        resp.request_focus();
        return NameEdit::Editing;
    }
    let (enter, escape, pressed) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::Enter),
            i.key_pressed(egui::Key::Escape),
            i.pointer.any_pressed(),
        )
    });
    let pointer_away = resp.clicked_elsewhere() || (resp.lost_focus() && pressed);
    let outcome = if pointer_away {
        NameEdit::Commit
    } else if ime_busy {
        if resp.lost_focus() && (enter || escape) {
            // egui let go for a key the conversion owns: take it back.
            resp.request_focus();
        }
        NameEdit::Editing
    } else if escape {
        NameEdit::Cancel
    } else if enter || resp.lost_focus() {
        NameEdit::Commit
    } else {
        NameEdit::Editing
    };
    if outcome != NameEdit::Editing {
        ui.memory_mut(|m| m.surrender_focus(resp.id));
    }
    outcome
}

#[derive(Clone, Copy)]
enum ClipCommand {
    SplitAtPlayhead,
    Duplicate,
    Delete,
}

/// What a click on a clip does to the selection: Ctrl toggles it, Shift adds
/// it, a plain click selects it alone.
fn clip_click_action(ui: &egui::Ui, id: &str) -> Action {
    let mods = click_mods(ui);
    if mods.ctrl || mods.command {
        Action::ToggleClip(id.to_string())
    } else if mods.shift {
        Action::AddClip(id.to_string())
    } else {
        Action::SelectClip(Some(id.to_string()))
    }
}

/// Both sets of modifiers held: either one says a key was down.
fn merge_mods(a: egui::Modifiers, b: egui::Modifiers) -> egui::Modifiers {
    egui::Modifiers {
        alt: a.alt || b.alt,
        ctrl: a.ctrl || b.ctrl,
        shift: a.shift || b.shift,
        mac_cmd: a.mac_cmd || b.mac_cmd,
        command: a.command || b.command,
    }
}

/// The modifiers held at a click: the frame's, and the release event's --
/// a test harness, and a fast click, carry them on the event alone.
fn click_mods(ui: &egui::Ui) -> egui::Modifiers {
    ui.input(|i| {
        i.events
            .iter()
            .filter_map(|event| match event {
                egui::Event::PointerButton {
                    pressed: false,
                    modifiers,
                    ..
                } => Some(*modifiers),
                _ => None,
            })
            .fold(i.modifiers, merge_mods)
    })
}

/// The corners of the part of clip `rect` from `xa` to `xb`: rounded only
/// where the part reaches the clip's own ends.
fn clip_corners(rect: Rect, xa: f32, xb: f32) -> egui::CornerRadius {
    let r = CLIP_CORNER as u8;
    let left = if xa <= rect.left() + 0.5 { r } else { 0 };
    let right = if xb >= rect.right() - 0.5 { r } else { 0 };
    egui::CornerRadius {
        nw: left,
        sw: left,
        ne: right,
        se: right,
    }
}

/// A crossfade between two clips over `seg`, drawn on the clip on top: its
/// own gain (rising for a fade-in) in the crossfade colour, the clip under
/// it in the waveform colour, as the equal-power curves both follow; and the
/// crossfade's length when there is room.
fn paint_crossfade(painter: &egui::Painter, rect: Rect, map: TimeMap, seg: &XfadeSeg) {
    let (a, b) = (map.x(seg.start), map.x(seg.end));
    if b - a < 1.0 {
        return;
    }
    let steps = ((b - a) / 3.0).ceil().clamp(4.0, 64.0) as usize;
    let curve = |rising: bool| -> Vec<Pos2> {
        (0..=steps)
            .map(|k| {
                let u = k as f32 / steps as f32;
                let phase = u * std::f32::consts::FRAC_PI_2;
                let gain = if rising { phase.sin() } else { phase.cos() };
                Pos2::new(a + (b - a) * u, rect.bottom() - gain * rect.height())
            })
            .collect()
    };
    let own_rising = seg.shape == XfadeShape::In;
    // The span itself, tinted, and where the clip underneath ends (its edge
    // is under this clip's see-through fill).
    painter.rect_filled(
        Rect::from_min_max(Pos2::new(a, rect.top()), Pos2::new(b, rect.bottom())),
        0.0,
        XFADE_COLOR.gamma_multiply(0.12),
    );
    if own_rising {
        painter.add(egui::Shape::dashed_line(
            &[Pos2::new(b, rect.top()), Pos2::new(b, rect.bottom())],
            Stroke::new(1.2, Color32::WHITE.gamma_multiply(0.8)),
            4.0,
            3.0,
        ));
    }
    painter.add(egui::Shape::line(curve(!own_rising), Stroke::new(2.0, WAVE_COLOR)));
    painter.add(egui::Shape::line(curve(own_rising), Stroke::new(2.0, XFADE_COLOR)));
    if b - a >= 56.0 {
        let label = format!("X {}", format_len(seg.end - seg.start));
        let galley = painter.layout_no_wrap(label, FontId::proportional(11.0), Color32::WHITE);
        let pos = Pos2::new((a + b) * 0.5 - galley.size().x * 0.5, rect.bottom() - galley.size().y - 3.0);
        let backing = Rect::from_min_size(pos - Vec2::new(3.0, 1.0), galley.size() + Vec2::new(6.0, 2.0));
        painter.rect_filled(backing, 3.0, Color32::from_black_alpha(150));
        painter.galley(pos, galley, Color32::WHITE);
    }
}

/// Widest a waiting clip's chip grows (its name is cut to fit).
const PENDING_CHIP_MAX_W: f32 = 180.0;
/// Height of a waiting clip's chip, and the step between stacked ones.
const PENDING_CHIP_H: f32 = 17.0;

/// A clip whose length is not known yet, as its start alone: a line down
/// `band` at `x` and a chip with its name and an ellipsis. Several waiting
/// at one spot stack their chips (`stack`), wrapping to the top when the
/// band is full. Returns the chip.
fn paint_pending_clip(
    painter: &egui::Painter,
    x: f32,
    band: Rect,
    stack: usize,
    name: &str,
    fill: Color32,
    text: Color32,
) -> Rect {
    painter.line_segment(
        [Pos2::new(x, band.top()), Pos2::new(x, band.bottom())],
        Stroke::new(2.0, fill),
    );
    let rows = ((band.height() - 4.0) / PENDING_CHIP_H).floor().max(1.0) as usize;
    let top = band.top() + 2.0 + (stack % rows) as f32 * PENDING_CHIP_H;
    let galley = painter.layout_no_wrap(format!("{name} \u{2026}"), FontId::proportional(11.0), text);
    let width = (galley.size().x + 10.0).min(PENDING_CHIP_MAX_W);
    let chip = Rect::from_min_size(Pos2::new(x, top), Vec2::new(width, PENDING_CHIP_H - 2.0));
    painter.rect_filled(chip, 3.0, fill);
    painter.add(egui::Shape::dashed_line(
        &[chip.left_top(), chip.right_top(), chip.right_bottom(), chip.left_bottom(), chip.left_top()],
        Stroke::new(1.0, Color32::WHITE.gamma_multiply(0.7)),
        3.0,
        2.0,
    ));
    painter
        .with_clip_rect(chip.shrink(1.0).intersect(painter.clip_rect()))
        .galley(Pos2::new(chip.left() + 5.0, chip.center().y - galley.size().y * 0.5), galley, text);
    chip
}

/// A length, short: "3.25 s", or "1:05.3" from a minute up.
/// What the output chip and its menu call output channel `index` of
/// `layout`: its speaker, else its number.
fn output_channel_name(layout: &[Option<SpeakerPos>], index: usize) -> String {
    match layout.get(index).copied().flatten() {
        Some(pos) => pos.label(layout).to_string(),
        None => format!("Ch {}", index + 1),
    }
}

fn format_len(secs: f64) -> String {
    let secs = secs.max(0.0);
    if secs < 60.0 {
        format!("{secs:.2} s")
    } else {
        let minutes = (secs / 60.0).floor() as u64;
        format!("{minutes}:{:04.1}", secs - minutes as f64 * 60.0)
    }
}

/// "m:ss.mmm", the way the ruler and the readout show time.
fn format_secs(secs: f64) -> String {
    let secs = secs.max(0.0);
    let minutes = (secs / 60.0).floor() as u64;
    let rest = secs - minutes as f64 * 60.0;
    format!("{minutes}:{rest:06.3}")
}

impl WavesPreviewer {
    /// One tab label per open timeline, in the workspace tab bar. A timeline
    /// the session does not hold as it is carries the editor's unsaved dot.
    pub(in crate::app) fn ui_multi_edit_tab_labels(&mut self, ui: &mut egui::Ui) {
        let open: Vec<(String, String)> = self
            .multi_edit
            .docs
            .iter()
            .filter(|doc| doc.open)
            .map(|doc| (doc.id.clone(), doc.name.clone()))
            .collect();
        for (id, name) in open {
            ui.horizontal(|ui| {
                let active = self.is_multi_edit_workspace_active()
                    && self.multi_edit.active.as_deref() == Some(id.as_str());
                let shown = if self.multi_edit_is_dirty(&id) {
                    format!("\u{25CF} {name}")
                } else {
                    name.clone()
                };
                let text = if active {
                    RichText::new(format!("[{shown}]")).strong()
                } else {
                    RichText::new(shown)
                };
                let resp = ui.selectable_label(active, text);
                if resp.clicked() && !active {
                    self.multi_edit_open(&id);
                }
                resp.context_menu(|ui| {
                    if ui.button("Rename...").clicked() {
                        self.multi_edit.ui.renaming_doc = Some((id.clone(), name.clone()));
                        self.multi_edit.ui.rename_focus_pending = true;
                        ui.close();
                    }
                    if ui.button("Delete timeline...").clicked() {
                        self.multi_edit.ui.confirm_delete_doc = Some(id.clone());
                        ui.close();
                    }
                });
                if ui
                    .button("x")
                    .on_hover_text("Close (the timeline stays in the session: Tools > Multi Edits)")
                    .clicked()
                {
                    self.multi_edit_close(&id);
                }
            });
        }
    }

    pub(in crate::app) fn ui_multi_edit_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.multi_edit_track_ime(ctx);
        egui::Panel::left("multi_edit_list_pane")
            .resizable(true)
            .default_size(320.0)
            .size_range(egui::Rangef::new(200.0, 720.0))
            .show_inside(ui, |ui| {
                self.ui_multi_edit_list_pane(ui, ctx);
            });
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.ui_multi_edit_timeline(ui, ctx);
        });
        self.ui_multi_edit_point_editor(ctx);
        self.ui_multi_edit_dialogs(ctx);
    }

    /// Follow the input method: whether it is composing, and whether its
    /// events touch this frame. A name field reads the result to tell the
    /// Enter that confirms a conversion from the one that confirms the name.
    fn multi_edit_track_ime(&mut self, ctx: &egui::Context) {
        let was_composing = self.multi_edit.ui.ime_composing;
        let mut composing = was_composing;
        let mut touched = false;
        ctx.input(|i| {
            for event in &i.events {
                if let egui::Event::Ime(ime) = event {
                    touched = true;
                    composing = match ime {
                        egui::ImeEvent::Preedit(text) => !text.is_empty(),
                        egui::ImeEvent::Commit(_) | egui::ImeEvent::Disabled => false,
                        egui::ImeEvent::Enabled => composing,
                    };
                }
            }
        });
        self.multi_edit.ui.ime_composing = composing;
        self.multi_edit.ui.ime_busy = was_composing || touched;
    }

    /// The list, drawn with the pane's columns. It is the List workspace's
    /// list: rows, order, filters and selection are the same state.
    fn ui_multi_edit_list_pane(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("List").strong());
            ui.menu_button("Columns", |ui| {
                let mut changed = false;
                for column in ColumnId::ALL {
                    if matches!(column, ColumnId::External) {
                        continue;
                    }
                    let mut on = column.enabled(&self.multi_edit_list_columns);
                    if ui.checkbox(&mut on, column.label()).changed() {
                        column.set_enabled(&mut self.multi_edit_list_columns, on);
                        changed = true;
                    }
                }
                if changed {
                    // A pane with no name column cannot be told apart.
                    self.multi_edit_list_columns.file = true;
                    self.save_prefs();
                }
            });
            ui.label(RichText::new("drag rows onto a track").weak().small());
        });
        let rect = ui.available_rect_before_wrap();
        let _scroll = self.pointer_scroll_input_guard(UiSurface::List, ctx);
        self.list_view_profile = ListViewProfile::MultiEditPane;
        self.ui_list_view(ui, ctx);
        self.list_view_profile = ListViewProfile::Main;
        self.ui_input_focus
            .register_region(UiSurface::List, ui.layer_id(), rect);
    }

    /// Measure the rows a drag from the list carries, once per drag, and
    /// forget them when it ends.
    fn multi_edit_refresh_drop_preview(&mut self, ctx: &egui::Context) {
        self.multi_edit.ui.drop_preview_shown = None;
        let Some(payload) = egui::DragAndDrop::payload::<MultiEditRowDrag>(ctx) else {
            self.multi_edit.ui.drop_preview = None;
            return;
        };
        let key = std::sync::Arc::as_ptr(&payload) as usize;
        if self
            .multi_edit
            .ui
            .drop_preview
            .as_ref()
            .is_some_and(|preview| preview.key == key)
        {
            return;
        }
        let clips = self.multi_edit_new_clips(&payload.0);
        self.multi_edit.ui.drop_preview = Some(DropPreview::new(key, clips));
    }

    fn ui_multi_edit_timeline(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if self.multi_edit_active_doc().is_none() {
            return;
        }
        self.ui_multi_edit_header(ui);
        ui.separator();
        let area = ui.available_rect_before_wrap();
        self.multi_edit_clamp_view(area.width() - HEADER_W);
        let Some(doc) = self.multi_edit_active_doc().cloned() else {
            return;
        };
        let mut actions: Vec<Action> = Vec::new();
        self.multi_edit_refresh_drop_preview(ctx);
        if let Some(mods) = ctx.input(|i| {
            i.events.iter().rev().find_map(|event| match event {
                egui::Event::PointerButton {
                    pressed: true,
                    modifiers,
                    ..
                } => Some(merge_mods(*modifiers, i.modifiers)),
                _ => None,
            })
        }) {
            self.multi_edit.ui.press_mods = mods;
        }
        self.ui_input_focus
            .register_region(UiSurface::MultiEdit, ui.layer_id(), area);
        let _scroll = self.pointer_scroll_input_guard(UiSurface::MultiEdit, ctx);
        let map = TimeMap {
            left: area.left() + HEADER_W,
            right: area.right(),
            px_per_sec: doc.view.px_per_sec.clamp(
                zoom_out_limit(doc.end_secs(), area.width() - HEADER_W),
                MAX_PX_PER_SEC,
            ),
            scroll_secs: doc.view.scroll_secs.max(0.0),
        };
        self.multi_edit.ui.lane_left = map.left;
        let playhead = self.multi_edit_playhead(&doc.id);
        let alt = ctx.input(|i| i.modifiers.alt);
        let snap = self.multi_edit_snap_set(&doc, map, playhead, alt);

        // Ruler, with the markers on it.
        let (ruler_row, _) =
            ui.allocate_exact_size(Vec2::new(area.width(), RULER_H), Sense::hover());
        let ruler = Rect::from_min_max(Pos2::new(map.left, ruler_row.top()), ruler_row.max);
        self.ui_multi_edit_ruler(ui, &doc, ruler, map, &snap, &mut actions);

        let body = Rect::from_min_max(Pos2::new(area.left(), ruler_row.bottom()), area.max);
        self.multi_edit_wheel(ctx, ui, body, map, &mut actions);

        let mut track_rows: Vec<(usize, Rect)> = Vec::new();
        let mut lane_rects: HashMap<(String, String), Rect> = HashMap::new();
        let output = egui::ScrollArea::vertical()
            .id_salt(("multi_edit_tracks", doc.id.as_str()))
            .auto_shrink([false, false])
            // The wheel is the timeline's own (see `multi_edit_wheel`); only
            // the bar scrolls here, and dragging on the rows edits them.
            .scroll_source(egui::scroll_area::ScrollSource::SCROLL_BAR)
            .vertical_scroll_offset(doc.view.scroll_y)
            .show(ui, |ui| {
                for (ti, track) in doc.tracks.iter().enumerate() {
                    let row =
                        self.ui_multi_edit_track_row(ui, &doc, ti, map, playhead, &snap, &mut actions);
                    track_rows.push((ti, row));
                    if !track.lanes_collapsed {
                        for lane in &track.lanes {
                            let rect = self.ui_multi_edit_lane_row(
                                ui, &doc, ti, lane, map, playhead, &snap, &mut actions,
                            );
                            lane_rects.insert((track.id.clone(), lane.id.clone()), rect);
                        }
                    }
                }
                self.ui_multi_edit_add_row(ui, map, &snap, &mut actions);
            });
        let max_scroll_y = (output.content_size.y - output.inner_rect.height()).max(0.0);
        self.multi_edit.ui.max_scroll_y = max_scroll_y;
        let offset = output.state.offset.y.clamp(0.0, max_scroll_y);
        if (offset - doc.view.scroll_y).abs() > 0.5 {
            actions.push(Action::ScrollYTo(offset));
        }
        // A track picked with the keys last frame, brought into view.
        if let Some(id) = self.multi_edit.ui.reveal_track.take() {
            let view = output.inner_rect;
            let row = doc
                .tracks
                .iter()
                .position(|t| t.id == id)
                .and_then(|ti| track_rows.iter().find(|(i, _)| *i == ti))
                .map(|(_, rect)| *rect);
            if let Some(row) = row {
                let target = if row.top() < view.top() {
                    offset - (view.top() - row.top())
                } else if row.bottom() > view.bottom() {
                    // Its bottom into view, but never its top out of it.
                    offset + (row.bottom() - view.bottom()).min(row.top() - view.top())
                } else {
                    offset
                };
                if (target - offset).abs() > 0.5 {
                    actions.push(Action::ScrollYTo(target.clamp(0.0, max_scroll_y)));
                }
            }
        }

        // Markers and the playhead over every row.
        let painter = ui.painter_at(Rect::from_min_max(Pos2::new(map.left, body.top()), body.max));
        for marker in &doc.markers {
            let x = map.x(marker.secs);
            painter.add(egui::Shape::dashed_line(
                &[Pos2::new(x, body.top()), Pos2::new(x, body.bottom())],
                Stroke::new(1.0, MARKER_COLOR.gamma_multiply(0.7)),
                4.0,
                4.0,
            ));
        }
        let px = map.x(playhead);
        if px >= map.left && px <= map.right {
            ui.painter_at(area).line_segment(
                [Pos2::new(px, ruler.top()), Pos2::new(px, area.bottom())],
                Stroke::new(1.5, PLAYHEAD_COLOR),
            );
        }

        self.multi_edit_timeline_keys(ctx, &doc, &mut actions);
        self.apply_multi_edit_actions(actions, map);
        self.multi_edit_continue_drags(ctx, &doc, map, &snap, &track_rows, &lane_rects);
        // The selection rectangle, over the rows.
        if let (Some(marquee), Some(pos)) = (
            self.multi_edit.ui.marquee.as_ref(),
            ctx.input(|i| i.pointer.interact_pos()),
        ) {
            let rect = Rect::from_two_pos(marquee.origin, pos).intersect(body);
            let selection = ui.visuals().selection;
            ui.painter_at(body).rect(
                rect,
                2.0,
                selection.bg_fill.gamma_multiply(0.18),
                Stroke::new(1.0, selection.stroke.color),
                StrokeKind::Inside,
            );
        }
    }

    /// Keep the view inside the timeline: zoomed out no further than the
    /// whole of it with a margin, scrolled no further than its end mid-view.
    fn multi_edit_clamp_view(&mut self, lane_width: f32) {
        self.multi_edit.ui.lane_width = lane_width.max(1.0);
        let max_y = self.multi_edit.ui.max_scroll_y;
        if let Some(doc) = self.multi_edit_active_doc_mut() {
            let end = doc.end_secs();
            let min_pps = zoom_out_limit(end, lane_width);
            doc.view.px_per_sec = doc.view.px_per_sec.clamp(min_pps, MAX_PX_PER_SEC);
            let visible = (lane_width / doc.view.px_per_sec) as f64;
            doc.view.scroll_secs = clamp_scroll_secs(doc.view.scroll_secs, end, visible);
            doc.view.scroll_y = doc.view.scroll_y.clamp(0.0, max_y);
            doc.view.track_zoom = doc.view.track_zoom.clamp(MIN_TRACK_ZOOM, MAX_TRACK_ZOOM);
        }
    }

    /// Everything a dragged or dropped time may snap to this frame.
    fn multi_edit_snap_set(&self, doc: &MultiEditDoc, map: TimeMap, playhead: f64, off: bool) -> Snap {
        let mut candidates = doc.snap_points(None);
        candidates.push(playhead);
        candidates.extend(doc.markers.iter().map(|m| m.secs));
        Snap {
            candidates,
            step: grid_step_secs(map.px_per_sec, MULTI_EDIT_GRID_MIN_PX),
            threshold: (SNAP_PX / map.px_per_sec) as f64,
            off,
        }
    }

    /// The timeline's wheel: plain scrolls time, Shift scrolls the tracks,
    /// Ctrl zooms time about the pointer, Ctrl+Shift zooms the rows.
    fn multi_edit_wheel(
        &mut self,
        ctx: &egui::Context,
        ui: &egui::Ui,
        body: Rect,
        map: TimeMap,
        actions: &mut Vec<Action>,
    ) {
        let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else {
            return;
        };
        if !body.contains(pos) || ctx.layer_id_at(pos) != Some(ui.layer_id()) {
            return;
        }
        let (dx, dy, zoom, held_shift, event_shift) = ctx.input(|i| {
            let event_shift = i.raw.events.iter().rev().find_map(|event| match event {
                egui::Event::MouseWheel { modifiers, .. } => Some(modifiers.shift),
                _ => None,
            });
            (
                i.smooth_scroll_delta.x,
                i.smooth_scroll_delta.y,
                i.zoom_delta(),
                i.modifiers.shift,
                event_shift,
            )
        });
        if let Some(shift) = event_shift {
            self.multi_edit.ui.wheel_shift = shift;
        }
        let shift = held_shift || self.multi_edit.ui.wheel_shift;
        if (zoom - 1.0).abs() > f32::EPSILON {
            if shift {
                actions.push(Action::ZoomVertical(zoom));
            } else {
                let x = pos.x.max(map.left);
                actions.push(Action::Zoom {
                    factor: zoom,
                    anchor_secs: map.secs(x),
                    anchor_x: x - map.left,
                });
            }
        }
        // egui already turned a Shift+wheel into a sideways delta.
        let delta = dx + dy;
        if delta.abs() > 0.0 {
            if shift {
                actions.push(Action::ScrollYBy(-delta));
            } else {
                actions.push(Action::Scroll(-(delta / map.px_per_sec) as f64));
            }
            ctx.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
        }
    }

    fn ui_multi_edit_header(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = self.multi_edit_active_doc().cloned() else {
            return;
        };
        ui.horizontal(|ui| {
            let renaming = self
                .multi_edit
                .ui
                .renaming_doc
                .as_ref()
                .is_some_and(|(id, _)| id == &doc.id);
            if renaming {
                let focus_now = std::mem::take(&mut self.multi_edit.ui.rename_focus_pending);
                let ime_busy = self.multi_edit.ui.ime_busy;
                let mut outcome = NameEdit::Editing;
                if let Some((_, text)) = self.multi_edit.ui.renaming_doc.as_mut() {
                    outcome = inline_name_field(ui, None, text, 200.0, focus_now, ime_busy);
                }
                if outcome != NameEdit::Editing {
                    if let Some((id, text)) = self.multi_edit.ui.renaming_doc.take() {
                        let name = text.trim().to_string();
                        if outcome == NameEdit::Commit && !name.is_empty() {
                            if let Some(doc) = self.multi_edit.doc_mut(&id) {
                                doc.name = name;
                            }
                            self.multi_edit_mark_changed(&id);
                        }
                    }
                }
            } else {
                let resp = ui
                    .add(egui::Label::new(RichText::new(&doc.name).strong()).sense(Sense::click()))
                    .on_hover_text("Double-click to rename");
                if resp.double_clicked() {
                    self.multi_edit.ui.renaming_doc = Some((doc.id.clone(), doc.name.clone()));
                    self.multi_edit.ui.rename_focus_pending = true;
                }
            }
            ui.separator();
            let playing = self.multi_edit_is_playing(&doc.id);
            if ui
                .button(if playing { "\u{25A0} Stop" } else { "\u{25B6} Play" })
                .on_hover_text("Space")
                .clicked()
            {
                self.multi_edit_toggle_play();
            }
            ui.label(
                RichText::new(format!(
                    "{} / {}",
                    format_secs(self.multi_edit_playhead(&doc.id)),
                    format_secs(doc.end_secs())
                ))
                .monospace(),
            );
            let loading = doc
                .sources()
                .iter()
                .filter(|s| matches!(self.multi_edit.sources.get(&s.path), Some(SourceSlot::Loading)))
                .count();
            if loading > 0 {
                ui.spinner();
                ui.label(RichText::new(format!("reading {loading} source(s)")).weak());
            }
            ui.separator();
            if ui.small_button("\u{2212}").on_hover_text("Zoom out (Ctrl+wheel)").clicked() {
                self.multi_edit_zoom_by(1.0 / ZOOM_BUTTON_STEP);
            }
            if ui.small_button("+").on_hover_text("Zoom in (Ctrl+wheel)").clicked() {
                self.multi_edit_zoom_by(ZOOM_BUTTON_STEP);
            }
            if ui
                .small_button("\u{2195}\u{2212}")
                .on_hover_text("Lower rows (Ctrl+Shift+wheel)")
                .clicked()
            {
                self.multi_edit_zoom_rows(1.0 / VERTICAL_ZOOM_STEP);
            }
            if ui
                .small_button("\u{2195}+")
                .on_hover_text("Taller rows (Ctrl+Shift+wheel)")
                .clicked()
            {
                self.multi_edit_zoom_rows(VERTICAL_ZOOM_STEP);
            }
            ui.separator();
            let cut = self.multi_edit.ui.cut_tool;
            if ui
                .selectable_label(cut, "\u{2702} Cut")
                .on_hover_text(
                    "Cut tool (C): click a clip to split it there. \
                     Alt+click splits a clip without the tool, and without snapping.",
                )
                .clicked()
            {
                self.multi_edit.ui.cut_tool = !cut;
            }
            let selected = self.multi_edit.ui.selected_clips.len();
            if selected > 1 {
                ui.label(RichText::new(format!("{selected} clips")).strong())
                    .on_hover_text("Delete, drag, Alt+arrows, Ctrl+C / X / D and S act on all of them. Esc clears.");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let exporting = self
                    .multi_edit
                    .export
                    .as_ref()
                    .filter(|job| job.doc_id == doc.id)
                    .map(|job| job.progress());
                match exporting {
                    Some(progress) => {
                        if ui.button("Cancel").clicked() {
                            self.multi_edit_cancel_export();
                        }
                        ui.add(
                            egui::ProgressBar::new(progress)
                                .desired_width(120.0)
                                .text("Mixing down"),
                        );
                    }
                    None => {
                        let pending = doc.has_pending();
                        let can = self.multi_edit.export.is_none() && doc.end_secs() > 0.0 && !pending;
                        if ui
                            .add_enabled(can, egui::Button::new("Export"))
                            .on_hover_text(
                                "Mix the timeline down, in its output format, into a new \
                                 (virtual) row in the list. Save it from there. Video tracks \
                                 contribute their sound only.",
                            )
                            .on_disabled_hover_text(if pending {
                                "Some clips are still waiting for their length to be read."
                            } else {
                                "Nothing to mix down yet."
                            })
                            .clicked()
                        {
                            self.multi_edit_start_export();
                        }
                    }
                }
                ui.separator();
                let current = doc.output_layout();
                let current_name = layout_name(&current);
                let mut chosen: Option<Layout> = None;
                egui::ComboBox::from_id_salt(("me_output", &doc.id))
                    .selected_text(&current_name)
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        for preset in PRESETS {
                            let layout: Layout = preset.speakers.iter().copied().map(Some).collect();
                            if ui.selectable_label(layout == current, preset.name).clicked() {
                                chosen = Some(layout);
                            }
                        }
                        // A format no preset has (a split of an unusual
                        // source) stays in the list while it is the one.
                        if !PRESETS.iter().any(|preset| preset.name == current_name) {
                            let _ = ui.selectable_label(true, &current_name);
                        }
                    })
                    .response
                    .on_hover_text(
                        "The mix's channels. A stereo track plays on the front pair; \
                         a mono track on the one channel its chip names. Export writes \
                         this many channels.",
                    );
                ui.label("Output");
                if let Some(layout) = chosen {
                    self.multi_edit_set_output_layout(layout);
                }
            });
        });
    }

    fn multi_edit_zoom_by(&mut self, factor: f32) {
        let playhead = self
            .multi_edit
            .active
            .as_deref()
            .map(|id| self.multi_edit_playhead(id))
            .unwrap_or(0.0);
        let width = self.multi_edit.ui.lane_width;
        if let Some(doc) = self.multi_edit_active_doc_mut() {
            let before = doc.view.px_per_sec;
            let after = (before * factor).clamp(zoom_out_limit(doc.end_secs(), width), MAX_PX_PER_SEC);
            // Keep the playhead where it is on screen.
            let offset = (playhead - doc.view.scroll_secs) * before as f64;
            doc.view.px_per_sec = after;
            doc.view.scroll_secs = (playhead - offset / after as f64).max(0.0);
        }
    }

    fn multi_edit_zoom_rows(&mut self, factor: f32) {
        let Some(id) = self.multi_edit.active.clone() else {
            return;
        };
        if let Some(doc) = self.multi_edit_active_doc_mut() {
            doc.view.track_zoom = (doc.view.track_zoom * factor).clamp(MIN_TRACK_ZOOM, MAX_TRACK_ZOOM);
        }
        self.multi_edit_mark_changed(&id);
    }

    fn ui_multi_edit_ruler(
        &mut self,
        ui: &mut egui::Ui,
        doc: &MultiEditDoc,
        ruler: Rect,
        map: TimeMap,
        snap: &Snap,
        actions: &mut Vec<Action>,
    ) {
        let painter = ui.painter_at(ruler);
        let visuals = ui.visuals().clone();
        painter.rect_filled(ruler, 0.0, visuals.extreme_bg_color);
        let step = grid_step_secs(map.px_per_sec, MULTI_EDIT_GRID_MIN_PX);
        let (start, end) = map.visible_secs();
        for t in grid_points(start - step, end, step) {
            let x = map.x(t);
            painter.line_segment(
                [Pos2::new(x, ruler.bottom() - 6.0), Pos2::new(x, ruler.bottom())],
                Stroke::new(1.0, visuals.weak_text_color()),
            );
            painter.text(
                Pos2::new(x + 3.0, ruler.bottom() - 9.0),
                Align2::LEFT_CENTER,
                format_secs(t),
                FontId::monospace(10.0),
                visuals.weak_text_color(),
            );
        }
        // The ruler seeks; clicks near a marker's flag belong to the marker.
        let ruler_resp = ui.interact(ruler, ui.id().with("multi_edit_ruler"), Sense::click_and_drag());
        let marker_busy = self.multi_edit.ui.marker_drag.is_some();
        if !marker_busy && (ruler_resp.clicked() || ruler_resp.dragged()) {
            if let Some(pos) = ruler_resp.interact_pointer_pos() {
                actions.push(Action::Seek(snap.apply(map.secs(pos.x), &[])));
            }
        }
        for marker in &doc.markers {
            let x = map.x(marker.secs);
            if x < ruler.left() - 40.0 || x > ruler.right() {
                continue;
            }
            let flag = [
                Pos2::new(x, ruler.top() + 2.0 + MARKER_FLAG),
                Pos2::new(x - MARKER_FLAG * 0.6, ruler.top() + 2.0),
                Pos2::new(x + MARKER_FLAG * 0.6, ruler.top() + 2.0),
            ];
            painter.add(egui::Shape::convex_polygon(flag.to_vec(), MARKER_COLOR, Stroke::NONE));
            let galley = painter.layout_no_wrap(
                marker.label.clone(),
                FontId::proportional(10.0),
                MARKER_COLOR,
            );
            let text_pos = Pos2::new(x + MARKER_FLAG * 0.6 + 2.0, ruler.top() + 1.0);
            let hit = Rect::from_min_max(
                Pos2::new(x - MARKER_FLAG, ruler.top()),
                Pos2::new(text_pos.x + galley.size().x + 2.0, ruler.top() + MARKER_FLAG + 6.0),
            );
            painter.galley(text_pos, galley, MARKER_COLOR);
            let resp = ui
                .interact(hit, ui.id().with(("me_marker", &marker.id)), Sense::click_and_drag())
                .on_hover_text(format!(
                    "{} at {} -- drag to move, double-click to rename, right-click for more",
                    marker.label,
                    format_secs(marker.secs)
                ));
            if resp.drag_started() {
                actions.push(Action::BeginMarkerDrag(marker.id.clone()));
            } else if resp.double_clicked() {
                actions.push(Action::BeginRenameMarker(marker.id.clone()));
            } else if resp.clicked() {
                actions.push(Action::Seek(marker.secs));
            }
            resp.context_menu(|ui| {
                if ui.button("Rename...").clicked() {
                    actions.push(Action::BeginRenameMarker(marker.id.clone()));
                    ui.close();
                }
                if ui.button("Delete marker").clicked() {
                    actions.push(Action::RemoveMarker(marker.id.clone()));
                    ui.close();
                }
            });
        }
        // Renaming a marker: a field on the ruler, where the flag is.
        let renaming = self.multi_edit.ui.renaming_marker.clone();
        if let Some((id, _)) = renaming {
            if let Some(marker) = doc.markers.iter().find(|m| m.id == id) {
                let x = map.x(marker.secs).clamp(ruler.left(), ruler.right() - 140.0);
                let field = Rect::from_min_size(Pos2::new(x, ruler.top()), Vec2::new(140.0, RULER_H - 4.0));
                let focus_now = std::mem::take(&mut self.multi_edit.ui.rename_focus_pending);
                let ime_busy = self.multi_edit.ui.ime_busy;
                let mut commit = None;
                let mut cancel = false;
                if let Some((_, text)) = self.multi_edit.ui.renaming_marker.as_mut() {
                    // In a child, so the ruler row's layout is not moved.
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(field));
                    match inline_name_field(&mut child, Some(field), text, 136.0, focus_now, ime_busy) {
                        NameEdit::Commit => commit = Some(text.clone()),
                        NameEdit::Cancel => cancel = true,
                        NameEdit::Editing => {}
                    }
                }
                if cancel {
                    self.multi_edit.ui.renaming_marker = None;
                }
                if let Some(label) = commit {
                    self.multi_edit.ui.renaming_marker = None;
                    self.multi_edit_checkpoint();
                    if let Some(doc) = self.multi_edit_active_doc_mut() {
                        doc.rename_marker(&id, &label);
                    }
                    let doc_id = doc.id.clone();
                    self.multi_edit_mark_changed(&doc_id);
                }
            } else {
                self.multi_edit.ui.renaming_marker = None;
            }
        }
    }

    /// Faint vertical lines at the ruler's ticks, the grid things snap to.
    fn paint_grid(painter: &egui::Painter, rect: Rect, map: TimeMap, color: Color32) {
        let step = grid_step_secs(map.px_per_sec, MULTI_EDIT_GRID_MIN_PX);
        let (start, end) = map.visible_secs();
        for t in grid_points(start, end, step) {
            let x = map.x(t);
            painter.line_segment(
                [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                Stroke::new(1.0, color),
            );
        }
    }

    /// One track: its header and its clip row. Returns the row's rect.
    #[allow(clippy::too_many_arguments)]
    fn ui_multi_edit_track_row(
        &mut self,
        ui: &mut egui::Ui,
        doc: &MultiEditDoc,
        ti: usize,
        map: TimeMap,
        playhead: f64,
        snap: &Snap,
        actions: &mut Vec<Action>,
    ) -> Rect {
        let track = &doc.tracks[ti];
        let height = track.height * doc.view.track_zoom;
        let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
        let header = Rect::from_min_max(row.min, Pos2::new(row.left() + HEADER_W, row.bottom()));
        let lane = Rect::from_min_max(Pos2::new(map.left, row.top()), row.max);
        let visuals = ui.visuals().clone();
        let line = visuals.widgets.noninteractive.bg_stroke.color;
        let selected = self.multi_edit.ui.selected_track.as_deref() == Some(track.id.as_str());
        let header_fill = if selected {
            visuals.selection.bg_fill.gamma_multiply(0.45)
        } else {
            visuals.faint_bg_color
        };
        ui.painter().rect_filled(header, 0.0, header_fill);
        ui.painter()
            .rect_filled(lane, 0.0, visuals.extreme_bg_color.gamma_multiply(0.6));
        Self::paint_grid(&ui.painter_at(lane), lane, map, line.gamma_multiply(0.35));
        ui.painter()
            .line_segment([row.left_bottom(), row.right_bottom()], Stroke::new(1.0, line));
        ui.painter()
            .line_segment([header.right_top(), header.right_bottom()], Stroke::new(1.0, line));

        // Header background: a click selects the track; the menu adds lanes.
        let header_resp = ui.interact(header, ui.id().with(("me_track_hdr", &track.id)), Sense::click());
        if header_resp.clicked() {
            actions.push(Action::SelectTrack(Some(track.id.clone())));
        }
        header_resp.context_menu(|ui| {
            ui.menu_button("Add lane", |ui| {
                for param in LaneParam::ALL {
                    let exists = track.lane(param).is_some();
                    let mono_pan = param == LaneParam::Pan && track.output.is_mono();
                    let mut resp = ui.add_enabled(!exists && !mono_pan, egui::Button::new(param.label()));
                    if mono_pan {
                        resp = resp.on_disabled_hover_text("A mono track has nothing to pan");
                    }
                    if resp.clicked() {
                        actions.push(Action::AddLane(ti, param));
                        ui.close();
                    }
                }
            });
            if ui.button("Rename").clicked() {
                actions.push(Action::BeginRenameTrack(ti));
                ui.close();
            }
            if ui.button("Delete track").clicked() {
                actions.push(Action::RemoveTrack(ti));
                ui.close();
            }
        });
        self.ui_multi_edit_track_header(ui, track, ti, header, &doc.output_layout(), actions);
        Self::resize_grip(
            ui,
            header,
            ("me_track_grip", &track.id),
            RowResize::Track {
                track_id: track.id.clone(),
                start_height: track.height,
                start_y: 0.0,
            },
            actions,
        );

        // The clip row: a click on empty space seeks and clears the
        // selection; rows dropped here join this track.
        // A click on empty space seeks; a drag from it selects the clips its
        // rectangle meets.
        let lane_resp = ui.interact(lane, ui.id().with(("me_lane", &track.id)), Sense::click_and_drag());
        if lane_resp.drag_started() {
            let origin = ui
                .input(|i| i.pointer.press_origin())
                .or_else(|| lane_resp.interact_pointer_pos());
            if let Some(origin) = origin {
                let mods = self.multi_edit.ui.press_mods;
                actions.push(Action::BeginMarquee {
                    origin,
                    additive: mods.shift || mods.ctrl || mods.command,
                });
            }
        }
        if lane_resp.clicked() {
            if let Some(pos) = lane_resp.interact_pointer_pos() {
                actions.push(Action::SelectClip(None));
                actions.push(Action::SelectTrack(None));
                actions.push(Action::Seek(snap.apply(map.secs(pos.x), &[])));
            }
        }
        let can_paste = self.multi_edit.ui.clip_clipboard.is_some();
        lane_resp.context_menu(|ui| {
            if ui
                .add_enabled(can_paste, egui::Button::new("Paste at playhead (Ctrl+V)"))
                .clicked()
            {
                actions.push(Action::Paste(Some(track.id.clone())));
                ui.close();
            }
        });
        let painter = ui.painter_at(lane);
        // Back to front by start: a clip that starts later lies over the one
        // it overlaps, see-through there (`covered_spans`).
        let xfades = crossfade_segments(&track.clips);
        let covered = covered_spans(&track.clips);
        for ci in draw_order(&track.clips) {
            self.ui_multi_edit_clip(
                ui,
                &painter,
                track.kind,
                &track.clips[ci],
                &xfades[ci],
                &covered[ci],
                lane,
                map,
                snap,
                actions,
            );
        }
        // Clips still waiting for a length, as their starts; several at one
        // spot stack.
        let mut stacks: Vec<(f64, usize)> = Vec::new();
        for clip in track.clips.iter().filter(|clip| clip.len_pending) {
            let stack = match stacks.iter_mut().find(|(t, _)| (t - clip.start_secs).abs() < 1e-6) {
                Some((_, n)) => {
                    *n += 1;
                    *n
                }
                None => {
                    stacks.push((clip.start_secs, 0));
                    0
                }
            };
            self.ui_multi_edit_pending_clip(ui, &painter, track.kind, clip, stack, lane, map, actions);
        }
        if track.lanes_collapsed && !track.lanes.is_empty() {
            Self::paint_folded_lanes(&painter, track, lane, map);
        }
        // Last, so the clips about to land are drawn over the ones here.
        self.multi_edit_drop_target(ui, lane, Some((ti, track.kind)), map, snap, actions);
        let _ = playhead;
        row
    }

    /// Name, fold arrow, output chip and video toggle on the first line;
    /// fader and M / S on the second, when the row is tall enough for it.
    /// Every control has a rect of its own inside the header, so nothing
    /// spills into the clips. `out` is the timeline's output format.
    fn ui_multi_edit_track_header(
        &mut self,
        ui: &mut egui::Ui,
        track: &Track,
        ti: usize,
        header: Rect,
        out: &[Option<SpeakerPos>],
        actions: &mut Vec<Action>,
    ) {
        // A child of the row that the row's layout never sees: placing a
        // widget in the parent would move its cursor back up to that widget,
        // and the next row would be laid over this one.
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(header));
        let ui = &mut child;
        let inner = header.shrink(HEADER_PAD);
        let line1 = Rect::from_min_size(inner.min, Vec2::new(inner.width(), HEADER_LINE_H));
        let mut left = line1.left();
        let mut right = line1.right();
        if !track.lanes.is_empty() {
            let rect = Rect::from_min_size(Pos2::new(left, line1.top()), Vec2::new(HEADER_BUTTON - 4.0, HEADER_LINE_H));
            let arrow = if track.lanes_collapsed { "\u{25B8}" } else { "\u{25BE}" };
            if ui
                .put(rect, egui::Button::new(arrow).small().frame(false))
                .on_hover_text(if track.lanes_collapsed {
                    "Show the lanes"
                } else {
                    "Fold the lanes (their curves are drawn over the clips)"
                })
                .clicked()
            {
                actions.push(Action::ToggleLanes(ti));
            }
            left = rect.right() + 2.0;
        }
        if track.kind == TrackKind::Video {
            let rect = Rect::from_min_max(Pos2::new(right - VIDEO_BUTTON_W, line1.top()), line1.right_bottom());
            if ui
                .put(
                    rect,
                    egui::Button::selectable(track.show_video, RichText::new("Video").small())
                        .truncate(),
                )
                .on_hover_text("Show or hide this track's picture window")
                .clicked()
            {
                actions.push(Action::ToggleVideo(track.id.clone(), !track.show_video));
            }
            right = rect.left() - 4.0;
        }
        // Where the track plays: the front pair, or one channel in mono.
        {
            let rect = Rect::from_min_max(Pos2::new(right - OUTPUT_CHIP_W, line1.top()), Pos2::new(right, line1.bottom()));
            let (text, hover) = match track.output {
                TrackOutput::Stereo => (
                    "St".to_string(),
                    "Stereo: plays on the output's front pair, and pans. Click to choose.".to_string(),
                ),
                TrackOutput::Channel { index } if index < out.len() => {
                    let name = output_channel_name(out, index);
                    let hover = format!("Mono: plays on {name} (channel {}) alone. Click to choose.", index + 1);
                    (name, hover)
                }
                TrackOutput::Channel { index } => (
                    format!("Ch {}", index + 1),
                    format!("Silent: the output has no channel {}. Click to choose.", index + 1),
                ),
            };
            let chip = ui
                .put(
                    rect,
                    egui::Button::selectable(track.output.is_mono(), RichText::new(text).small()).truncate(),
                )
                .on_hover_text(hover);
            egui::Popup::menu(&chip).show(|ui| {
                if ui
                    .selectable_label(track.output == TrackOutput::Stereo, "Stereo (front pair)")
                    .clicked()
                {
                    actions.push(Action::SetTrackOutput(track.id.clone(), TrackOutput::Stereo));
                    ui.close();
                }
                ui.separator();
                for index in 0..out.len() {
                    let output = TrackOutput::Channel { index };
                    let label = format!("Mono \u{2192} {}", output_channel_name(out, index));
                    if ui.selectable_label(track.output == output, label).clicked() {
                        actions.push(Action::SetTrackOutput(track.id.clone(), output));
                        ui.close();
                    }
                }
            });
            right = rect.left() - 4.0;
        }
        let name_rect = Rect::from_min_max(Pos2::new(left, line1.top()), Pos2::new(right.max(left + 10.0), line1.bottom()));
        let renaming = self
            .multi_edit
            .ui
            .renaming_track
            .as_ref()
            .is_some_and(|(id, _)| id == &track.id);
        if renaming {
            let focus_now = std::mem::take(&mut self.multi_edit.ui.rename_focus_pending);
            let ime_busy = self.multi_edit.ui.ime_busy;
            if let Some((_, text)) = self.multi_edit.ui.renaming_track.as_mut() {
                match inline_name_field(ui, Some(name_rect), text, name_rect.width(), focus_now, ime_busy) {
                    NameEdit::Commit => actions.push(Action::CommitRenameTrack(ti, text.clone())),
                    NameEdit::Cancel => actions.push(Action::CancelRenameTrack),
                    NameEdit::Editing => {}
                }
            }
        } else {
            // Left-aligned, as in the design (`put` would centre it).
            let name = ui
                .scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(name_rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&track.name).strong())
                                .truncate()
                                .sense(Sense::click()),
                        )
                    },
                )
                .inner
                .on_hover_text(format!(
                    "{} -- double-click to rename; right-click for lanes",
                    track.name
                ));
            if name.double_clicked() {
                actions.push(Action::BeginRenameTrack(ti));
            } else if name.clicked() {
                actions.push(Action::SelectTrack(Some(track.id.clone())));
            }
        }

        // The second line only when there is room for it.
        if inner.height() < HEADER_LINE_H * 2.0 + 4.0 {
            return;
        }
        let line2 = Rect::from_min_size(
            Pos2::new(inner.left(), line1.bottom() + 4.0),
            Vec2::new(inner.width(), HEADER_LINE_H),
        );
        let solo = Rect::from_min_max(Pos2::new(line2.right() - HEADER_BUTTON, line2.top()), line2.right_bottom());
        let mute = solo.translate(Vec2::new(-(HEADER_BUTTON + 2.0), 0.0));
        let fader = Rect::from_min_max(line2.min, Pos2::new(mute.left() - 6.0, line2.bottom()));
        if ui
            .put(mute, egui::Button::selectable(track.mute, "M").small())
            .on_hover_text("Mute")
            .clicked()
        {
            actions.push(Action::ToggleMute(ti));
        }
        if ui
            .put(solo, egui::Button::selectable(track.solo, "S").small())
            .on_hover_text("Solo")
            .clicked()
        {
            actions.push(Action::ToggleSolo(ti));
        }
        ui.scope_builder(egui::UiBuilder::new().max_rect(fader), |ui| {
            ui.spacing_mut().slider_width = fader.width().max(10.0);
            let mut volume = track.volume_db;
            let slider = ui
                .add(
                    egui::Slider::new(&mut volume, TRACK_GAIN_MIN_DB..=TRACK_GAIN_MAX_DB)
                        .show_value(false)
                        .trailing_fill(true),
                )
                .on_hover_text(format!("{volume:+.1} dB (double-click: 0 dB)"));
            if slider.drag_started() || (slider.clicked() && !slider.dragged()) {
                actions.push(Action::Checkpoint);
            }
            if slider.double_clicked() {
                actions.push(Action::Checkpoint);
                actions.push(Action::SetVolume(ti, 0.0));
            } else if slider.changed() {
                actions.push(Action::SetVolume(ti, volume));
            }
        });
    }

    /// The strip at the bottom of a header that resizes its row.
    fn resize_grip(
        ui: &egui::Ui,
        header: Rect,
        id: impl std::hash::Hash,
        resize: RowResize,
        actions: &mut Vec<Action>,
    ) {
        let grip = Rect::from_min_max(
            Pos2::new(header.left(), header.bottom() - RESIZE_GRAB_PX),
            header.right_bottom(),
        );
        let resp = ui.interact(grip, ui.id().with(id), Sense::drag());
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
            ui.painter().line_segment(
                [Pos2::new(grip.left(), grip.center().y), Pos2::new(grip.right(), grip.center().y)],
                Stroke::new(2.0, ui.visuals().selection.bg_fill),
            );
        }
        if resp.drag_started() {
            let y = resp.interact_pointer_pos().map(|p| p.y).unwrap_or(grip.center().y);
            let resize = match resize {
                RowResize::Track { track_id, start_height, .. } => RowResize::Track {
                    track_id,
                    start_height,
                    start_y: y,
                },
                RowResize::Lane { track_id, lane_id, start_height, .. } => RowResize::Lane {
                    track_id,
                    lane_id,
                    start_height,
                    start_y: y,
                },
            };
            actions.push(Action::BeginResize(resize));
        }
    }

    /// Folded lanes: each parameter's curve over the clips, in its colour,
    /// with a small legend.
    fn paint_folded_lanes(painter: &egui::Painter, track: &Track, lane: Rect, map: TimeMap) {
        let inner = lane.shrink2(Vec2::new(0.0, 6.0));
        let mut legend_x = lane.right() - 6.0;
        for automation in track.lanes.iter().rev() {
            let color = param_color(automation.param);
            Self::paint_curve(painter, automation, inner, map, color.gamma_multiply(0.85), 1.5, 2.5);
            let galley = painter.layout_no_wrap(
                automation.param.label().to_string(),
                FontId::proportional(10.0),
                color,
            );
            legend_x -= galley.size().x;
            painter.galley(Pos2::new(legend_x, lane.top() + 2.0), galley, color);
            legend_x -= 6.0;
        }
    }

    /// A lane's curve in `rect`: flat before its first point and after its
    /// last, stepped for Mute, with its points.
    fn paint_curve(
        painter: &egui::Painter,
        lane: &AutomationLane,
        rect: Rect,
        map: TimeMap,
        color: Color32,
        width: f32,
        point_radius: f32,
    ) {
        let (lo, hi) = lane.param.range();
        let y_of = |v: f32| rect.bottom() - (v - lo) / (hi - lo) * rect.height();
        let (vis0, vis1) = map.visible_secs();
        if lane.points.is_empty() {
            let y = y_of(lane.param.neutral());
            painter.line_segment(
                [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                Stroke::new(width, color.gamma_multiply(0.5)),
            );
            return;
        }
        let mut pts: Vec<Pos2> = Vec::new();
        pts.push(Pos2::new(map.x(vis0.min(lane.points[0].secs)), y_of(lane.points[0].value)));
        for (i, point) in lane.points.iter().enumerate() {
            let x = map.x(point.secs);
            if lane.param.is_stepped() && i > 0 {
                pts.push(Pos2::new(x, y_of(lane.points[i - 1].value)));
            }
            pts.push(Pos2::new(x, y_of(point.value)));
        }
        let last = lane.points[lane.points.len() - 1];
        pts.push(Pos2::new(map.x(vis1.max(last.secs)), y_of(last.value)));
        painter.add(egui::Shape::line(pts, Stroke::new(width, color)));
        if point_radius > 0.0 {
            for point in &lane.points {
                let c = Pos2::new(map.x(point.secs), y_of(point.value));
                painter.circle_filled(c, point_radius, color);
            }
        }
    }

    /// Accept rows dragged from the list pane over `rect` (a track's clip
    /// row, or the row under the tracks when `track` is `None`), drawing the
    /// clips they would become where they would land: each one's name and
    /// length, and the span they cover from start to end.
    ///
    /// The span snaps by whichever end is nearer a snap point, the way a
    /// dragged clip does, and the drop lands exactly where it is drawn.
    ///
    /// Tested against the rect itself rather than a response: a drop onto a
    /// clip is a drop onto its track, and a response is "covered" wherever a
    /// clip sits on it.
    fn multi_edit_drop_target(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        track: Option<(usize, TrackKind)>,
        map: TimeMap,
        snap: &Snap,
        actions: &mut Vec<Action>,
    ) {
        let ctx = ui.ctx().clone();
        if !egui::DragAndDrop::has_payload_of_type::<MultiEditRowDrag>(&ctx) {
            return;
        }
        let Some(pos) = ctx.pointer_hover_pos().or_else(|| ctx.pointer_interact_pos()) else {
            return;
        };
        if !rect.contains(pos) || ctx.layer_id_at(pos) != Some(ui.layer_id()) {
            return;
        }
        let Some(preview) = self.multi_edit.ui.drop_preview.as_ref() else {
            return;
        };
        // The group that lands here: the track's own kind when there is any
        // of it, otherwise whatever there is (which goes to its own track).
        let own_kind = track.map(|(_, kind)| kind).unwrap_or(TrackKind::Audio);
        let other_kind = match own_kind {
            TrackKind::Audio => TrackKind::Video,
            TrackKind::Video => TrackKind::Audio,
        };
        let (main_kind, rest_kind) = if preview.group(own_kind).0.is_empty() {
            (other_kind, own_kind)
        } else {
            (own_kind, other_kind)
        };
        let (_, main_secs) = preview.group(main_kind);
        let (_, rest_secs) = preview.group(rest_kind);
        let start = snap
            .apply_span(map.secs(pos.x.max(map.left)), main_secs, &[])
            .max(0.0);
        let span = main_secs.max(rest_secs);
        let count = preview.count();

        let visuals = ui.visuals();
        let accent = visuals.selection.bg_fill;
        let text = visuals.strong_text_color();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, accent.gamma_multiply(0.08));
        painter.rect_stroke(rect, 0.0, Stroke::new(1.5, accent), StrokeKind::Inside);
        let inner = rect.shrink2(Vec2::new(0.0, 3.0));
        let split = !preview.group(rest_kind).0.is_empty();
        let main_band = if split {
            Rect::from_min_max(inner.min, Pos2::new(inner.right(), inner.top() + inner.height() * 0.62))
        } else {
            inner
        };
        let destination = |kind: TrackKind| {
            let name = match kind {
                TrackKind::Audio => "Audio track",
                TrackKind::Video => "Video track",
            };
            if track.is_none() {
                format!("new {name}")
            } else {
                name.to_string()
            }
        };
        let main_note = (track.is_none() || main_kind != own_kind).then(|| destination(main_kind));
        Self::paint_drop_group(&painter, main_band, map, start, preview.group(main_kind).0, main_kind, 1.0, main_note, text);
        if split {
            let rest_band = Rect::from_min_max(Pos2::new(inner.left(), main_band.bottom() + 2.0), inner.max);
            Self::paint_drop_group(
                &painter,
                rest_band,
                map,
                start,
                preview.group(rest_kind).0,
                rest_kind,
                0.45,
                Some(destination(rest_kind)),
                text,
            );
        }
        // Where the span starts and ends, through the whole row.
        let (x0, x1) = (map.x(start), map.x(start + span));
        for x in [x0, x1] {
            painter.line_segment(
                [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                Stroke::new(2.0, accent),
            );
        }
        // The summary sits above the row, on a layer of its own, so a short
        // row or the edge of the scroll area never cuts it.
        let mut summary = format!(
            "{} \u{2013} {}   {}   {} clip{}",
            format_secs(start),
            format_secs(start + span),
            format_len(span),
            count,
            if count == 1 { "" } else { "s" },
        );
        if preview.pending > 0 {
            summary.push_str(&format!(" ({} not read yet)", preview.pending));
        }
        let label_painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            egui::Id::new("multi_edit_drop_summary"),
        ));
        let galley = label_painter.layout_no_wrap(summary, FontId::proportional(12.0), text);
        let size = galley.size() + Vec2::new(12.0, 6.0);
        let left = x0.clamp(map.left, (map.right - size.x).max(map.left));
        let pill = Rect::from_min_size(Pos2::new(left, rect.top() - size.y - 2.0), size);
        label_painter.rect_filled(pill, 4.0, visuals.extreme_bg_color);
        label_painter.rect_stroke(pill, 4.0, Stroke::new(1.0, accent), StrokeKind::Inside);
        label_painter.galley(pill.min + Vec2::new(6.0, 3.0), galley, text);
        self.multi_edit.ui.drop_preview_shown = Some((start, span, count));

        if ctx.input(|i| i.pointer.any_released()) {
            if let Some(payload) = egui::DragAndDrop::take_payload::<MultiEditRowDrag>(&ctx) {
                actions.push(Action::DropRows {
                    track: track.map(|(ti, _)| ti),
                    at_secs: start,
                    paths: payload.0.clone(),
                });
            }
        }
    }

    /// One group of clips about to be dropped, back to back from `start`,
    /// translucent, each with its name and length. `note` says where the
    /// group goes when that is not the row it is drawn on.
    #[allow(clippy::too_many_arguments)]
    fn paint_drop_group(
        painter: &egui::Painter,
        band: Rect,
        map: TimeMap,
        start: f64,
        clips: &[(String, Option<f64>)],
        kind: TrackKind,
        opacity: f32,
        note: Option<String>,
        text: Color32,
    ) {
        let fill = match kind {
            TrackKind::Audio => AUDIO_CLIP_FILL,
            TrackKind::Video => VIDEO_CLIP_FILL,
        };
        let text = text.gamma_multiply(opacity.max(0.6));
        let clip_rect = painter.clip_rect();
        let mut cursor = start;
        // Rows without a length land on one spot; their chips stack.
        let mut stacked = 0usize;
        for (i, (name, len)) in clips.iter().enumerate() {
            let Some(len) = len else {
                let x = map.x(cursor);
                if x >= clip_rect.left() - PENDING_CHIP_MAX_W && x <= clip_rect.right() {
                    let title = match (&note, i) {
                        (Some(note), 0) => format!("\u{2192} {note}   {name}"),
                        _ => name.clone(),
                    };
                    paint_pending_clip(painter, x, band, stacked, &title, fill.gamma_multiply(opacity), text);
                }
                stacked += 1;
                continue;
            };
            stacked = 0;
            let (x0, x1) = (map.x(cursor), map.x(cursor + len));
            cursor += len;
            if x1 < clip_rect.left() {
                continue;
            }
            if x0 > clip_rect.right() {
                // Everything after this is further right still.
                break;
            }
            let ghost = Rect::from_min_max(Pos2::new(x0, band.top()), Pos2::new(x1.max(x0 + 2.0), band.bottom()));
            painter.rect_filled(ghost, CLIP_CORNER, fill.gamma_multiply(0.55 * opacity));
            painter.rect_stroke(
                ghost,
                CLIP_CORNER,
                Stroke::new(1.5, Color32::WHITE.gamma_multiply(0.7 * opacity)),
                StrokeKind::Inside,
            );
            let room = ghost.shrink2(Vec2::new(5.0, 2.0)).intersect(clip_rect);
            if room.width() < 24.0 {
                continue;
            }
            let inside = painter.with_clip_rect(room);
            let title = match (&note, i) {
                (Some(note), 0) => format!("\u{2192} {note}   {name}"),
                _ => name.clone(),
            };
            // On a backing, as a placed clip's name is: the clips already
            // on the track show through the ghost.
            let backed = |pos: Pos2, line: String, size: f32| {
                let galley = inside.layout_no_wrap(line, FontId::proportional(size), text);
                let backing = Rect::from_min_size(pos - Vec2::new(3.0, 1.0), galley.size() + Vec2::new(6.0, 2.0));
                inside.rect_filled(backing, 3.0, Color32::from_black_alpha((150.0 * opacity) as u8));
                let height = galley.size().y;
                inside.galley(pos, galley, text);
                height
            };
            let top = room.left_top() + Vec2::new(3.0, 1.0);
            if room.height() >= 30.0 {
                let height = backed(top, title, 12.0);
                backed(top + Vec2::new(0.0, height + 2.0), format_len(*len), 11.0);
            } else {
                backed(top, format!("{title}  {}", format_len(*len)), 11.0);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn ui_multi_edit_clip(
        &mut self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        kind: TrackKind,
        clip: &Clip,
        xfades: &[XfadeSeg],
        covered: &[(f64, f64)],
        lane: Rect,
        map: TimeMap,
        snap: &Snap,
        actions: &mut Vec<Action>,
    ) {
        let x0 = map.x(clip.start_secs);
        let x1 = map.x(clip.end_secs());
        if x1 < lane.left() || x0 > lane.right() {
            return;
        }
        let rect = Rect::from_min_max(
            Pos2::new(x0, lane.top() + 4.0),
            Pos2::new(x1.max(x0 + 2.0), lane.bottom() - 4.0),
        );
        let selected = self.multi_edit.ui.selected_clips.contains(&clip.id);
        let primary = selected && self.multi_edit.ui.selected_clip.as_deref() == Some(clip.id.as_str());
        let poster = (kind == TrackKind::Video)
            .then(|| {
                self.item_for_path(&clip.source.path)
                    .and_then(|item| item.meta.as_ref())
                    .and_then(|meta| meta.cover_art.clone())
            })
            .flatten()
            .map(|art| self.list_art_texture_for_path(ui.ctx(), &clip.source.path, art));
        let slot = self.multi_edit.sources.get(&clip.source.path);
        let base = match kind {
            TrackKind::Audio => AUDIO_CLIP_FILL,
            TrackKind::Video => VIDEO_CLIP_FILL,
        };
        let (fill, status) = match slot {
            Some(SourceSlot::Failed(err)) => (MISSING_CLIP_FILL, Some(format!("missing: {err}"))),
            Some(SourceSlot::NotInList) => (MISSING_CLIP_FILL, Some("not in the list".to_string())),
            Some(SourceSlot::Loading) | None => (base, Some("reading...".to_string())),
            Some(SourceSlot::Ready(_)) => (base, None),
        };
        // Where the clip lies over one drawn before it, it is see-through, so
        // the clip under it -- waveform, name, edge -- stays in sight.
        let over = |t: f64| covered.iter().any(|&(s, e)| t >= s && t < e);
        let mut parts: Vec<(f32, f32, f32)> = Vec::with_capacity(covered.len() * 2 + 1);
        let mut x = rect.left();
        for &(s, e) in covered {
            let (cs, ce) = (map.x(s).max(rect.left()), map.x(e).min(rect.right()));
            parts.push((x, cs, 0.85));
            parts.push((cs, ce, COVER_FILL_ALPHA));
            x = x.max(ce);
        }
        parts.push((x, rect.right(), 0.85));
        for (xa, xb, alpha) in parts {
            if xb > xa {
                let part = Rect::from_min_max(Pos2::new(xa, rect.top()), Pos2::new(xb, rect.bottom()));
                painter.rect_filled(part, clip_corners(rect, xa, xb), fill.gamma_multiply(alpha));
            }
        }
        // A video clip opens with its poster frame, when the list has one.
        let mut wave_left = rect.left();
        if let Some(texture) = poster {
            let tex = texture.size_vec2().max(Vec2::splat(1.0));
            let h = rect.height() - 4.0;
            let w = (h * tex.x / tex.y).min(rect.width() * 0.5);
            let thumb = Rect::from_min_size(rect.min + Vec2::new(2.0, 2.0), Vec2::new(w, h));
            let tint = if over(clip.start_secs) {
                Color32::WHITE.gamma_multiply(0.5)
            } else {
                Color32::WHITE
            };
            painter.image(
                texture.id(),
                thumb,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                tint,
            );
            wave_left = thumb.right() + 2.0;
        }

        // Waveform of the part of the clip on screen, shaped by its fades.
        if let Some(SourceSlot::Ready(source)) = slot {
            let vis_x0 = wave_left.max(lane.left());
            let vis_x1 = rect.right().min(lane.right());
            let width = (vis_x1 - vis_x0).max(0.0) as usize;
            if width > 0 {
                let sr = source.playback.sample_rate.max(1) as f64;
                let secs_at = |x: f32| clip.in_secs + (map.secs(x) - clip.start_secs);
                let s0 = (secs_at(vis_x0) * sr).max(0.0) as usize;
                let s1 = (secs_at(vis_x1) * sr).max(0.0) as usize;
                let mut peaks = Vec::new();
                source
                    .peaks_for(clip.channel)
                    .query_columns(s0, s1.max(s0 + 1), width, 0.0, &mut peaks);
                let mid = rect.center().y;
                let half = rect.height() * 0.45;
                for (i, peak) in peaks.iter().enumerate() {
                    let x = vis_x0 + i as f32 + 0.5;
                    let t = map.secs(x);
                    let gain = fade_gain_at(clip, t) * xfade_gain(xfades, t);
                    let top = mid - peak.max.clamp(-1.0, 1.0) * half * gain;
                    let bottom = mid - peak.min.clamp(-1.0, 1.0) * half * gain;
                    let color = if over(t) {
                        COVER_WAVE_COLOR.gamma_multiply(0.85)
                    } else {
                        WAVE_COLOR.gamma_multiply(0.8)
                    };
                    painter.line_segment(
                        [Pos2::new(x, top), Pos2::new(x, bottom.max(top + 1.0))],
                        Stroke::new(1.0, color),
                    );
                }
            }
        }

        // Fades as the design has them: a line from the bottom corner up to
        // where the fade ends. Crossfades with a neighbour draw an X.
        let fade_stroke = Stroke::new(1.2, Color32::WHITE.gamma_multiply(0.8));
        if clip.fade_in_secs > 0.0 {
            let fx = map.x(clip.start_secs + clip.fade_in_secs);
            painter.line_segment([rect.left_bottom(), Pos2::new(fx, rect.top())], fade_stroke);
        }
        if clip.fade_out_secs > 0.0 {
            let fx = map.x(clip.end_secs() - clip.fade_out_secs);
            painter.line_segment([Pos2::new(fx, rect.top()), rect.right_bottom()], fade_stroke);
        }
        for seg in xfades {
            match seg.shape {
                // Under a clip laid wholly over it: silent there.
                XfadeShape::Zero => {
                    let (a, b) = (map.x(seg.start), map.x(seg.end));
                    painter.rect_filled(
                        Rect::from_min_max(Pos2::new(a, rect.top()), Pos2::new(b, rect.bottom())),
                        0.0,
                        Color32::from_black_alpha(90),
                    );
                }
                // A crossfade is drawn once, by the clip on top, over both.
                XfadeShape::In | XfadeShape::Out => {
                    if over((seg.start + seg.end) * 0.5) {
                        paint_crossfade(painter, rect, map, seg);
                    }
                }
            }
        }
        let outline = if selected {
            Stroke::new(if primary { 2.5 } else { 2.0 }, ui.visuals().selection.stroke.color)
        } else {
            Stroke::new(1.0, Color32::from_gray(200).gamma_multiply(0.6))
        };
        painter.rect_stroke(rect, CLIP_CORNER, outline, StrokeKind::Inside);

        // The name, on a backing so the fade lines never run through it, cut
        // to the clip.
        let name = match clip.channel {
            Some(ch) => format!("{} ({})", clip.name, self.multi_edit_source_channel_name(&clip.source.path, ch)),
            None => clip.name.clone(),
        };
        let label = match &status {
            Some(status) => format!("{name}  ({status})"),
            None => name,
        };
        let visible = rect.intersect(lane);
        let text_pos = Pos2::new(visible.left() + FADE_HANDLE_PX + 4.0, rect.top() + 3.0);
        if visible.width() > FADE_HANDLE_PX * 2.0 + 12.0 {
            let clipped = painter.with_clip_rect(visible.shrink(1.0));
            let galley = clipped.layout_no_wrap(label, FontId::proportional(11.0), Color32::WHITE);
            let backing = Rect::from_min_size(text_pos - Vec2::new(3.0, 1.0), galley.size() + Vec2::new(6.0, 2.0));
            clipped.rect_filled(backing, 3.0, Color32::from_black_alpha(140));
            clipped.galley(text_pos, galley, Color32::WHITE);
        }

        // Interaction: body moves, edges trim, the top corners set fades.
        // Later rects win where they overlap, so the smallest go last.
        let id = ui.id().with(("me_clip", &clip.id));
        let cutting = self.multi_edit.ui.cut_tool;
        let sense = if cutting {
            Sense::click()
        } else {
            Sense::click_and_drag()
        };
        let body = ui.interact(rect, id, sense);
        // A click with Alt held splits the clip where it lands, unsnapped.
        // The release event carries the modifiers held at the click.
        let alt_click = body.clicked() && click_mods(ui).alt;
        if cutting || alt_click {
            if cutting {
                if let Some(pos) = body.hover_pos() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                    let raw = map.secs(pos.x);
                    let at = if ui.input(|i| i.modifiers.alt) {
                        raw
                    } else {
                        snap.apply(raw, &[])
                    };
                    let x = map.x(at);
                    painter.line_segment(
                        [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                        Stroke::new(2.0, CUT_LINE_COLOR),
                    );
                }
            }
            if body.clicked() {
                if let Some(pos) = body.interact_pointer_pos() {
                    let raw = map.secs(pos.x);
                    let at = if alt_click { raw } else { snap.apply(raw, &[]) };
                    actions.push(Action::SplitAt(clip.id.clone(), at));
                }
            }
            self.ui_multi_edit_clip_menu(&body, clip, actions);
            return;
        }
        let left_edge = Rect::from_min_max(rect.min, Pos2::new(rect.left() + EDGE_GRAB_PX, rect.bottom()));
        let right_edge = Rect::from_min_max(Pos2::new(rect.right() - EDGE_GRAB_PX, rect.top()), rect.max);
        let left = ui.interact(left_edge, id.with("l"), Sense::drag());
        let right = ui.interact(right_edge, id.with("r"), Sense::drag());
        let fade_in_x = map.x(clip.start_secs + clip.fade_in_secs).max(rect.left());
        let fade_out_x = map.x(clip.end_secs() - clip.fade_out_secs).min(rect.right());
        let fade_in_handle = Rect::from_center_size(
            Pos2::new(fade_in_x + FADE_HANDLE_PX * 0.5, rect.top() + FADE_HANDLE_PX * 0.5),
            Vec2::splat(FADE_HANDLE_PX),
        );
        let fade_out_handle = Rect::from_center_size(
            Pos2::new(fade_out_x - FADE_HANDLE_PX * 0.5, rect.top() + FADE_HANDLE_PX * 0.5),
            Vec2::splat(FADE_HANDLE_PX),
        );
        let fade_in = ui.interact(fade_in_handle, id.with("fi"), Sense::drag());
        let fade_out = ui.interact(fade_out_handle, id.with("fo"), Sense::drag());
        if body.hovered() || selected {
            for handle in [fade_in_handle, fade_out_handle] {
                painter.rect_filled(handle, 2.0, Color32::WHITE.gamma_multiply(0.7));
            }
        }
        for resp in [&left, &right] {
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
        }
        let begin = |kind: ClipDragKind| {
            Action::BeginClipDrag(ClipDrag {
                clip_id: clip.id.clone(),
                kind,
                group: Vec::new(),
            })
        };
        if fade_in.drag_started() {
            actions.push(begin(ClipDragKind::FadeIn));
        } else if fade_out.drag_started() {
            actions.push(begin(ClipDragKind::FadeOut));
        } else if left.drag_started() {
            actions.push(begin(ClipDragKind::TrimStart));
        } else if right.drag_started() {
            actions.push(begin(ClipDragKind::TrimEnd));
        } else if body.drag_started() {
            // Where on the clip it was pressed, not where the pointer is once
            // the drag threshold is passed: the clip must not jump.
            let press = ui
                .input(|i| i.pointer.press_origin())
                .or_else(|| body.interact_pointer_pos());
            let grab = press
                .map(|pos| map.secs(pos.x) - clip.start_secs)
                .unwrap_or(0.0);
            actions.push(begin(ClipDragKind::Move { grab_secs: grab }));
        } else if body.clicked() {
            actions.push(clip_click_action(ui, &clip.id));
        }
        if body.hovered() && self.multi_edit.ui.clip_drag.is_none() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        self.ui_multi_edit_clip_menu(&body, clip, actions);
    }

    /// A clip's right-click menu.
    fn ui_multi_edit_clip_menu(&self, body: &egui::Response, clip: &Clip, actions: &mut Vec<Action>) {
        let can_paste = self.multi_edit.ui.clip_clipboard.is_some();
        // On a selected clip the menu acts on the whole selection.
        let count = if self.multi_edit.ui.selected_clips.contains(&clip.id) {
            self.multi_edit.ui.selected_clips.len()
        } else {
            1
        };
        body.context_menu(|ui| {
            if count > 1 {
                ui.label(RichText::new(format!("{count} clips selected")).weak());
                ui.separator();
            }
            if ui.button("Copy (Ctrl+C)").clicked() {
                actions.push(Action::CopyClip(clip.id.clone()));
                ui.close();
            }
            if ui.button("Cut (Ctrl+X)").clicked() {
                actions.push(Action::CutClip(clip.id.clone()));
                ui.close();
            }
            if ui
                .add_enabled(can_paste, egui::Button::new("Paste at playhead (Ctrl+V)"))
                .clicked()
            {
                actions.push(Action::Paste(None));
                ui.close();
            }
            ui.separator();
            if ui.button("Split at playhead (S)").clicked() {
                actions.push(Action::ClipCommand(clip.id.clone(), ClipCommand::SplitAtPlayhead));
                ui.close();
            }
            if ui.button("Duplicate (Ctrl+D)").clicked() {
                actions.push(Action::ClipCommand(clip.id.clone(), ClipCommand::Duplicate));
                ui.close();
            }
            if ui.button("Delete (Del)").clicked() {
                actions.push(Action::ClipCommand(clip.id.clone(), ClipCommand::Delete));
                ui.close();
            }
            ui.separator();
            // This clip alone, whatever else is selected.
            let refusal = self.multi_edit_split_by_channel_refusal(&clip.id);
            let label = match self.multi_edit_clip_source_channels(&clip.id) {
                Some(channels) if channels > 1 => format!("Split into channels ({channels})"),
                _ => "Split into channels".to_string(),
            };
            if ui
                .add_enabled(refusal.is_none(), egui::Button::new(label))
                .on_hover_text(
                    "One track per channel of this clip's source, right below this \
                     track, each playing its channel in mono to the same speaker of \
                     the output",
                )
                .on_disabled_hover_text(format!("Not split: {}", refusal.unwrap_or("")))
                .clicked()
            {
                actions.push(Action::SplitChannels(clip.id.clone()));
                ui.close();
            }
        });
    }

    /// A clip whose row's length is not known yet: its start and a chip,
    /// which selects, moves and deletes it like a clip's body.
    #[allow(clippy::too_many_arguments)]
    fn ui_multi_edit_pending_clip(
        &mut self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        kind: TrackKind,
        clip: &Clip,
        stack: usize,
        lane: Rect,
        map: TimeMap,
        actions: &mut Vec<Action>,
    ) {
        let x = map.x(clip.start_secs);
        if x < lane.left() - PENDING_CHIP_MAX_W || x > lane.right() {
            return;
        }
        let fill = match kind {
            TrackKind::Audio => AUDIO_CLIP_FILL,
            TrackKind::Video => VIDEO_CLIP_FILL,
        };
        let band = lane.shrink2(Vec2::new(0.0, 4.0));
        let chip = paint_pending_clip(painter, x, band, stack, &clip.name, fill.gamma_multiply(0.9), Color32::WHITE);
        if self.multi_edit.ui.selected_clips.contains(&clip.id) {
            painter.rect_stroke(
                chip.expand(1.0),
                3.0,
                Stroke::new(2.0, ui.visuals().selection.stroke.color),
                StrokeKind::Outside,
            );
        }
        let body = ui
            .interact(chip, ui.id().with(("me_clip", &clip.id)), Sense::click_and_drag())
            .on_hover_text("Reading this row's length; the clip takes its full length once it is known.");
        if body.drag_started() {
            let press = ui
                .input(|i| i.pointer.press_origin())
                .or_else(|| body.interact_pointer_pos());
            let grab = press
                .map(|pos| map.secs(pos.x) - clip.start_secs)
                .unwrap_or(0.0);
            actions.push(Action::BeginClipDrag(ClipDrag {
                clip_id: clip.id.clone(),
                kind: ClipDragKind::Move { grab_secs: grab },
                group: Vec::new(),
            }));
        } else if body.clicked() {
            actions.push(clip_click_action(ui, &clip.id));
        }
        if body.hovered() && self.multi_edit.ui.clip_drag.is_none() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        self.ui_multi_edit_clip_menu(&body, clip, actions);
    }

    /// An automation lane under its track. Returns the lane's value rect.
    #[allow(clippy::too_many_arguments)]
    fn ui_multi_edit_lane_row(
        &mut self,
        ui: &mut egui::Ui,
        doc: &MultiEditDoc,
        ti: usize,
        lane: &AutomationLane,
        map: TimeMap,
        playhead: f64,
        snap: &Snap,
        actions: &mut Vec<Action>,
    ) -> Rect {
        let track = &doc.tracks[ti];
        let height = lane.height * doc.view.track_zoom;
        let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
        let header = Rect::from_min_max(row.min, Pos2::new(row.left() + HEADER_W, row.bottom()));
        let area = Rect::from_min_max(Pos2::new(map.left, row.top()), row.max);
        let visuals = ui.visuals().clone();
        let line = visuals.widgets.noninteractive.bg_stroke.color;
        let color = param_color(lane.param);
        ui.painter().rect_filled(header, 0.0, visuals.faint_bg_color);
        ui.painter().rect_filled(area, 0.0, visuals.extreme_bg_color.gamma_multiply(0.35));
        ui.painter()
            .line_segment([row.left_bottom(), row.right_bottom()], Stroke::new(1.0, line));
        ui.painter()
            .line_segment([header.right_top(), header.right_bottom()], Stroke::new(1.0, line));

        // Header: the bracket tying the lane to its track, the name, the value
        // at the playhead and the remove button, each in its own place.
        let hp = ui.painter_at(header);
        let bracket_x = header.left() + LANE_INDENT * 0.5;
        hp.line_segment(
            [Pos2::new(bracket_x, row.top()), Pos2::new(bracket_x, row.bottom())],
            Stroke::new(1.0, line),
        );
        let remove = Rect::from_center_size(
            Pos2::new(header.right() - HEADER_PAD - 8.0, header.center().y),
            Vec2::splat(16.0),
        );
        hp.text(
            Pos2::new(header.left() + LANE_INDENT + 4.0, header.center().y),
            Align2::LEFT_CENTER,
            lane.param.label(),
            FontId::proportional(13.0),
            color,
        );
        let unheard = lane.param == LaneParam::Pan && track.output.is_mono();
        hp.text(
            Pos2::new(remove.left() - 6.0, header.center().y),
            Align2::RIGHT_CENTER,
            if unheard {
                "mono: no pan".to_string()
            } else {
                lane.param.format_value(lane.value_at(playhead))
            },
            FontId::monospace(10.0),
            visuals.weak_text_color(),
        );
        let remove_resp = ui
            .interact(remove, ui.id().with(("me_lane_rm", &lane.id)), Sense::click())
            .on_hover_text("Remove this lane");
        hp.text(
            remove.center(),
            Align2::CENTER_CENTER,
            "\u{00D7}",
            FontId::proportional(14.0),
            if remove_resp.hovered() {
                visuals.strong_text_color()
            } else {
                visuals.weak_text_color()
            },
        );
        if remove_resp.clicked() {
            actions.push(Action::RemoveLane(ti, lane.id.clone()));
        }
        Self::resize_grip(
            ui,
            header,
            ("me_lane_grip", &lane.id),
            RowResize::Lane {
                track_id: track.id.clone(),
                lane_id: lane.id.clone(),
                start_height: lane.height,
                start_y: 0.0,
            },
            actions,
        );

        // The value scale, the grid, the curve, the points.
        let inner = area.shrink2(Vec2::new(0.0, 6.0));
        let (lo, hi) = lane.param.range();
        let y_of = |v: f32| inner.bottom() - (v - lo) / (hi - lo) * inner.height();
        let value_of = |y: f32| lane.param.clamp(lo + (inner.bottom() - y) / inner.height() * (hi - lo));
        let painter = ui.painter_at(area);
        Self::paint_grid(&painter, area, map, line.gamma_multiply(0.3));
        // Every line is drawn; a label only where it has room, the neutral
        // value's first, so a short lane shows fewer labels, never a pile.
        let mut ticks = lane_ticks(lane.param);
        ticks.sort_by(|a, b| {
            (a.0 - lane.param.neutral())
                .abs()
                .total_cmp(&(b.0 - lane.param.neutral()).abs())
        });
        let mut labelled: Vec<f32> = Vec::new();
        for (value, label) in ticks {
            let y = y_of(value);
            let strong = (value - lane.param.neutral()).abs() < f32::EPSILON;
            painter.line_segment(
                [Pos2::new(area.left(), y), Pos2::new(area.right(), y)],
                Stroke::new(1.0, line.gamma_multiply(if strong { 0.9 } else { 0.4 })),
            );
            let fits = y - LANE_LABEL_H >= area.top() && labelled.iter().all(|other| (other - y).abs() >= LANE_LABEL_H);
            if fits {
                labelled.push(y);
                painter.text(
                    Pos2::new(area.left() + 3.0, y - 1.0),
                    Align2::LEFT_BOTTOM,
                    label,
                    FontId::monospace(9.0),
                    visuals.weak_text_color().gamma_multiply(0.8),
                );
            }
        }
        Self::paint_curve(&painter, lane, inner, map, color, LANE_STROKE, 0.0);

        let resp = ui.interact(area, ui.id().with(("me_lane_area", &lane.id)), Sense::click_and_drag());
        let pointer = resp.interact_pointer_pos().or_else(|| resp.hover_pos());
        let nearest = pointer.and_then(|pos| {
            lane.points
                .iter()
                .enumerate()
                .map(|(i, p)| (i, Pos2::new(map.x(p.secs), y_of(p.value)).distance(pos)))
                .filter(|(_, d)| *d <= POINT_GRAB_PX)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i)
        });
        let dragging_here = self
            .multi_edit
            .ui
            .lane_drag
            .as_ref()
            .filter(|drag| drag.lane_id == lane.id)
            .map(|drag| drag.point);
        for (i, point) in lane.points.iter().enumerate() {
            let hot = nearest == Some(i) || dragging_here == Some(i);
            let c = Pos2::new(map.x(point.secs), y_of(point.value));
            let r = if hot { POINT_RADIUS_HOT } else { POINT_RADIUS };
            painter.circle_filled(c, r, color);
            painter.circle_stroke(c, r, Stroke::new(1.0, Color32::from_black_alpha(200)));
        }
        if let (Some(pos), true) = (resp.hover_pos(), resp.hovered()) {
            let text = match nearest {
                Some(i) => format!(
                    "{} at {} -- drag to move, double- or right-click to type a value",
                    lane.param.format_value(lane.points[i].value),
                    format_secs(lane.points[i].secs)
                ),
                None => format!("click to add {}", lane.param.format_value(value_of(pos.y))),
            };
            resp.clone().on_hover_text_at_pointer(text);
        }
        // A point is taken the moment it is pressed, so it moves with the
        // pointer from the first pixel instead of after the drag threshold.
        let pressed_now = ui.input(|i| i.pointer.primary_pressed());
        if pressed_now && resp.is_pointer_button_down_on() && dragging_here.is_none() {
            if let Some(point) = nearest {
                actions.push(Action::BeginPointDrag(LaneDrag {
                    track_id: track.id.clone(),
                    lane_id: lane.id.clone(),
                    point,
                }));
            }
        }
        if resp.double_clicked() || resp.secondary_clicked() {
            if let (Some(point), Some(pos)) = (nearest, pointer) {
                actions.push(Action::OpenPointEditor(PointEditor {
                    track_id: track.id.clone(),
                    lane_id: lane.id.clone(),
                    point,
                    secs: lane.points[point].secs,
                    value: lane.points[point].value,
                    at: pos + Vec2::new(12.0, 12.0),
                }));
            }
        } else if resp.clicked() {
            if let (Some(pos), None) = (pointer, nearest) {
                actions.push(Action::InsertPoint {
                    track: ti,
                    lane: lane.id.clone(),
                    secs: snap.apply(map.secs(pos.x), &[]),
                    value: value_of(pos.y),
                });
            }
        }
        inner
    }

    /// The row under the tracks: the (+) button, and a drop zone that makes
    /// a new track.
    fn ui_multi_edit_add_row(&mut self, ui: &mut egui::Ui, map: TimeMap, snap: &Snap, actions: &mut Vec<Action>) {
        let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ADD_ROW_H), Sense::hover());
        let area = Rect::from_min_max(Pos2::new(map.left, row.top()), row.max);
        let center = area.center();
        let button_rect = Rect::from_center_size(center, Vec2::splat(30.0));
        let button = ui.interact(button_rect, ui.id().with("me_add_track"), Sense::click());
        let visuals = ui.visuals();
        let color = if button.hovered() {
            visuals.strong_text_color()
        } else {
            visuals.text_color()
        };
        ui.painter().circle_stroke(center, 14.0, Stroke::new(1.5, color));
        ui.painter().text(center, Align2::CENTER_CENTER, "+", FontId::proportional(20.0), color);
        let button = button.on_hover_text("Add a track (drop rows here for a new one)");
        egui::Popup::menu(&button).show(|ui| {
            if ui.button("Audio track").clicked() {
                actions.push(Action::AddTrack(TrackKind::Audio));
                ui.close();
            }
            if ui.button("Video track").clicked() {
                actions.push(Action::AddTrack(TrackKind::Video));
                ui.close();
            }
        });
        // After the (+), so a drag over the row shows the clips over it.
        self.multi_edit_drop_target(ui, area, None, map, snap, actions);
    }

    /// The value popup of an automation point: its time and value typed in,
    /// or the point deleted.
    fn ui_multi_edit_point_editor(&mut self, ctx: &egui::Context) {
        let Some(editor) = self.multi_edit.ui.point_editor.clone() else {
            return;
        };
        let Some(param) = self
            .multi_edit_active_doc()
            .and_then(|doc| doc.tracks.iter().find(|t| t.id == editor.track_id))
            .and_then(|track| track.lanes.iter().find(|l| l.id == editor.lane_id))
            .filter(|lane| editor.point < lane.points.len())
            .map(|lane| lane.param)
        else {
            self.multi_edit.ui.point_editor = None;
            return;
        };
        let (lo, hi) = param.range();
        let mut edited = editor.clone();
        let mut close = false;
        let mut delete = false;
        let area = egui::Area::new(egui::Id::new("multi_edit_point_editor"))
            .order(egui::Order::Foreground)
            .fixed_pos(editor.at)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(RichText::new(format!("{} point", param.label())).strong());
                    egui::Grid::new("multi_edit_point_grid").num_columns(2).show(ui, |ui| {
                        ui.label("Time");
                        ui.add(
                            egui::DragValue::new(&mut edited.secs)
                                .speed(0.01)
                                .range(0.0..=f64::MAX)
                                .custom_formatter(|v, _| format_secs(v))
                                .custom_parser(parse_secs),
                        );
                        ui.end_row();
                        ui.label("Value");
                        if param.is_stepped() {
                            let mut muted = edited.value >= 0.5;
                            if ui.checkbox(&mut muted, "Muted").changed() {
                                edited.value = if muted { 1.0 } else { 0.0 };
                            }
                        } else {
                            let (speed, suffix) = match param {
                                LaneParam::Gain => (0.1, " dB"),
                                LaneParam::Pitch => (0.05, " st"),
                                _ => (0.01, ""),
                            };
                            ui.add(
                                egui::DragValue::new(&mut edited.value)
                                    .speed(speed)
                                    .range(lo..=hi)
                                    .suffix(suffix),
                            );
                        }
                        ui.end_row();
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Delete point").clicked() {
                            delete = true;
                        }
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    });
                });
            });
        let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
            && ctx
                .input(|i| i.pointer.interact_pos())
                .is_some_and(|pos| !area.response.rect.contains(pos))
            && !self.multi_edit.ui.point_editor_just_opened;
        self.multi_edit.ui.point_editor_just_opened = false;
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
        if delete {
            if let Some(lane) = self
                .multi_edit_active_doc_mut()
                .and_then(|doc| doc.tracks.iter_mut().find(|t| t.id == editor.track_id))
                .and_then(|track| track.lanes.iter_mut().find(|l| l.id == editor.lane_id))
            {
                lane.remove_point(editor.point);
            }
            self.multi_edit_touched();
            self.multi_edit.ui.point_editor = None;
            return;
        }
        if edited.secs != editor.secs || edited.value != editor.value {
            self.multi_edit_set_point(
                &editor.track_id,
                &editor.lane_id,
                editor.point,
                edited.secs,
                edited.value,
            );
            // Read back what the lane kept: neighbours clamp the time.
            let kept = self
                .multi_edit_active_doc()
                .and_then(|doc| doc.tracks.iter().find(|t| t.id == editor.track_id))
                .and_then(|track| track.lanes.iter().find(|l| l.id == editor.lane_id))
                .and_then(|lane| lane.points.get(editor.point).copied());
            if let Some(point) = kept {
                edited.secs = point.secs;
                edited.value = point.value;
            }
        }
        self.multi_edit.ui.point_editor = if close || clicked_outside {
            None
        } else {
            Some(edited)
        };
    }

    /// The timeline's keys, while it owns them: Delete, S, Ctrl+D, the marker
    /// key, and the arrows (the playhead; with Alt the selected clip).
    fn multi_edit_timeline_keys(&mut self, ctx: &egui::Context, doc: &MultiEditDoc, actions: &mut Vec<Action>) {
        if !self.surface_keys_allowed(UiSurface::MultiEdit) {
            self.multi_edit.ui.seek_hold = None;
            self.multi_edit.ui.nudge_hold = None;
            return;
        }
        if self.keymap_consume(ctx, crate::app::keymap::Action::EditorAddMarker) {
            actions.push(Action::AddMarker);
        }
        let delete = ctx.input_mut(|i| {
            i.consume_key(egui::Modifiers::NONE, egui::Key::Delete)
                | i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace)
        });
        if delete {
            actions.push(Action::DeleteSelected);
        }
        // Up and down pick a track; taken so egui's focus navigation does not
        // walk the keyboard off the timeline with them.
        let (up, down) = ctx.input_mut(|i| {
            (
                i.count_and_consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.count_and_consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
            )
        });
        let steps = down as i32 - up as i32;
        if steps != 0 {
            actions.push(Action::StepTrack(steps));
        }
        let cut = self.multi_edit.ui.cut_tool;
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::C)) {
            actions.push(Action::SetCutTool(!cut));
        } else if cut && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            actions.push(Action::SetCutTool(false));
        } else if !self.multi_edit.ui.selected_clips.is_empty()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            actions.push(Action::SelectClip(None));
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::A)) {
            actions.push(Action::SelectAllClips);
        }
        if let Some(clip) = self
            .multi_edit
            .ui
            .selected_clip
            .clone()
            .filter(|id| doc.clip(id).is_some())
        {
            let (split, duplicate) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::S),
                    i.consume_key(egui::Modifiers::COMMAND, egui::Key::D),
                )
            });
            if split {
                actions.push(Action::ClipCommand(clip, ClipCommand::SplitAtPlayhead));
            } else if duplicate {
                actions.push(Action::ClipCommand(clip, ClipCommand::Duplicate));
            }
        }
        if self.topbar_volume_owns_arrows(ctx) {
            return;
        }
        self.multi_edit_arrow_keys(ctx);
    }

    /// Held arrows repeat the way the editor's do (`SEEK_REPEAT_*`): one
    /// step on the press, then after a pause, then faster.
    fn multi_edit_arrow_keys(&mut self, ctx: &egui::Context) {
        use crate::app::types::SeekHoldState;
        use crate::app::ui_timing::{
            SEEK_REPEAT_ACCELERATE_AFTER, SEEK_REPEAT_DELAY, SEEK_REPEAT_FAST, SEEK_REPEAT_SLOW,
        };
        let (left_down, right_down, pressed_left, pressed_right, mods) = ctx.input(|i| {
            // The modifiers the arrow press itself carried, when there is one:
            // they are what was held at that press, however the frame's
            // modifier state reads by the time it is drawn.
            let pressed_with = i.events.iter().rev().find_map(|event| match event {
                egui::Event::Key {
                    key: egui::Key::ArrowLeft | egui::Key::ArrowRight,
                    pressed: true,
                    modifiers,
                    ..
                } => Some(*modifiers),
                _ => None,
            });
            (
                i.key_down(egui::Key::ArrowLeft),
                i.key_down(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
                pressed_with.unwrap_or(i.modifiers),
            )
        });
        // Taken so egui's focus navigation does not also act on them.
        ctx.input_mut(|i| {
            for key in [egui::Key::ArrowLeft, egui::Key::ArrowRight] {
                for m in [
                    egui::Modifiers::NONE,
                    egui::Modifiers::ALT,
                    egui::Modifiers::COMMAND,
                    egui::Modifiers::ALT | egui::Modifiers::COMMAND,
                ] {
                    i.consume_key(m, key);
                }
            }
        });
        // A tap can press and release within one frame: count the press too.
        let dir = match (left_down || pressed_left, right_down || pressed_right) {
            (true, false) => -1,
            (false, true) => 1,
            _ => 0,
        };
        let nudge = mods.alt;
        let fine = mods.ctrl || mods.command;
        let slot = if nudge {
            self.multi_edit.ui.seek_hold = None;
            &mut self.multi_edit.ui.nudge_hold
        } else {
            self.multi_edit.ui.nudge_hold = None;
            &mut self.multi_edit.ui.seek_hold
        };
        if dir == 0 {
            *slot = None;
            return;
        }
        let now = std::time::Instant::now();
        let pressed = if dir > 0 { pressed_right } else { pressed_left };
        let (step, first) = match slot.take() {
            Some(state) if state.dir == dir => {
                let elapsed = now.saturating_duration_since(state.started_at);
                let since = now.saturating_duration_since(state.last_step_at);
                let interval = if elapsed >= SEEK_REPEAT_ACCELERATE_AFTER {
                    SEEK_REPEAT_FAST
                } else {
                    SEEK_REPEAT_SLOW
                };
                let step = pressed || (elapsed >= SEEK_REPEAT_DELAY && since >= interval);
                *slot = Some(SeekHoldState {
                    last_step_at: if step { now } else { state.last_step_at },
                    ..state
                });
                (step, false)
            }
            _ => {
                *slot = Some(SeekHoldState {
                    dir,
                    started_at: now,
                    last_step_at: now,
                });
                (true, true)
            }
        };
        if step {
            if nudge {
                self.multi_edit_nudge_selected_clip(dir, fine, first);
            } else {
                self.multi_edit_step_playhead(dir, fine);
            }
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(SEEK_REPEAT_FAST);
        }
    }

    fn apply_multi_edit_actions(&mut self, actions: Vec<Action>, map: TimeMap) {
        let doc_id = self.multi_edit.active.clone().unwrap_or_default();
        for action in actions {
            match action {
                Action::SelectClip(id) => self.multi_edit_select_clip(id),
                Action::SelectTrack(id) => self.multi_edit_select_track(id),
                Action::BeginClipDrag(mut drag) => {
                    self.multi_edit_checkpoint();
                    // A selected clip carries the selection with it; any
                    // other clip is selected alone first.
                    if self.multi_edit.ui.selected_clips.contains(&drag.clip_id) {
                        self.multi_edit.ui.selected_clip = Some(drag.clip_id.clone());
                    } else {
                        self.multi_edit_select_clip(Some(drag.clip_id.clone()));
                    }
                    if matches!(drag.kind, ClipDragKind::Move { .. }) {
                        drag.group = self.multi_edit_selection_origins();
                    }
                    self.multi_edit.ui.clip_drag = Some(drag);
                }
                Action::ClipCommand(id, command) => {
                    // On a clip outside the selection, the command takes that
                    // clip alone; on a selected one, every selected clip.
                    if !self.multi_edit.ui.selected_clips.contains(&id) {
                        self.multi_edit_select_clip(Some(id.clone()));
                    }
                    let ids = self.multi_edit_selected_ids();
                    let playhead = self.multi_edit_playhead(&doc_id);
                    match command {
                        ClipCommand::Delete => {
                            self.multi_edit_delete_selected();
                        }
                        ClipCommand::SplitAtPlayhead => {
                            let can = self
                                .multi_edit_active_doc()
                                .is_some_and(|doc| ids.iter().any(|id| doc.can_split(id, playhead)));
                            if can {
                                self.multi_edit_checkpoint();
                                if let Some(doc) = self.multi_edit_active_doc_mut() {
                                    doc.split_clips_at(&ids, playhead);
                                }
                                self.multi_edit_touched();
                            }
                        }
                        ClipCommand::Duplicate => {
                            self.multi_edit_checkpoint();
                            let copies = self
                                .multi_edit_active_doc_mut()
                                .map(|doc| doc.duplicate_clips(&ids))
                                .unwrap_or_default();
                            let first = copies.first().cloned();
                            self.multi_edit_select_clips(copies, first);
                            self.multi_edit_touched();
                        }
                    }
                }
                Action::ToggleClip(id) => self.multi_edit_toggle_clip(id),
                Action::AddClip(id) => {
                    let ids: Vec<String> = self.multi_edit.ui.selected_clips.iter().cloned().collect();
                    self.multi_edit_select_clips(ids, Some(id));
                }
                Action::SelectAllClips => {
                    let ids: Vec<String> = self
                        .multi_edit_active_doc()
                        .map(|doc| {
                            doc.tracks
                                .iter()
                                .flat_map(|t| t.clips.iter().map(|c| c.id.clone()))
                                .collect()
                        })
                        .unwrap_or_default();
                    self.multi_edit_select_clips(ids, None);
                }
                Action::BeginMarquee { origin, additive } => {
                    let base = if additive {
                        self.multi_edit.ui.selected_clips.clone()
                    } else {
                        self.multi_edit_select_clip(None);
                        HashSet::new()
                    };
                    self.multi_edit.ui.marquee = Some(Marquee {
                        origin,
                        additive,
                        base,
                    });
                }
                Action::DeleteSelected => {
                    self.multi_edit_delete_selected();
                }
                Action::SplitAt(id, at) => {
                    let can = self
                        .multi_edit_active_doc()
                        .is_some_and(|doc| doc.can_split(&id, at));
                    if can {
                        self.multi_edit_checkpoint();
                        if let Some(doc) = self.multi_edit_active_doc_mut() {
                            doc.split_clip(&id, at);
                        }
                        self.multi_edit_select_clip(Some(id));
                        self.multi_edit_touched();
                    }
                }
                Action::SetCutTool(on) => self.multi_edit.ui.cut_tool = on,
                Action::StepTrack(steps) => self.multi_edit_step_track(steps),
                Action::CopyClip(id) => {
                    if !self.multi_edit.ui.selected_clips.contains(&id) {
                        self.multi_edit_select_clip(Some(id));
                    }
                    self.multi_edit_copy_selected();
                }
                Action::CutClip(id) => {
                    if !self.multi_edit.ui.selected_clips.contains(&id) {
                        self.multi_edit_select_clip(Some(id));
                    }
                    self.multi_edit_cut_selected();
                }
                Action::Paste(track) => {
                    if track.is_some() {
                        self.multi_edit_select_track(track);
                    }
                    self.multi_edit_paste();
                }
                Action::Seek(secs) => self.multi_edit_seek(secs),
                Action::DropRows {
                    track,
                    at_secs,
                    paths,
                } => {
                    self.multi_edit_drop_paths(track, at_secs, &paths);
                }
                Action::Checkpoint => self.multi_edit_checkpoint(),
                Action::SetVolume(ti, db) => {
                    if let Some(track) = self
                        .multi_edit_active_doc_mut()
                        .and_then(|doc| doc.tracks.get_mut(ti))
                    {
                        track.volume_db = db.clamp(TRACK_GAIN_MIN_DB, TRACK_GAIN_MAX_DB);
                    }
                    self.multi_edit_touched();
                }
                Action::ToggleMute(ti) => self.multi_edit_toggle_track_flag(ti, true),
                Action::ToggleSolo(ti) => self.multi_edit_toggle_track_flag(ti, false),
                Action::ToggleLanes(ti) => {
                    if let Some(track) = self
                        .multi_edit_active_doc_mut()
                        .and_then(|doc| doc.tracks.get_mut(ti))
                    {
                        track.lanes_collapsed = !track.lanes_collapsed;
                    }
                    self.multi_edit_mark_changed(&doc_id);
                }
                Action::ToggleVideo(track_id, show) => self.multi_edit_set_show_video(&track_id, show),
                Action::BeginRenameTrack(ti) => {
                    if let Some(track) = self.multi_edit_active_doc().and_then(|doc| doc.tracks.get(ti)) {
                        self.multi_edit.ui.renaming_track = Some((track.id.clone(), track.name.clone()));
                        self.multi_edit.ui.rename_focus_pending = true;
                    }
                }
                Action::CancelRenameTrack => self.multi_edit.ui.renaming_track = None,
                Action::CommitRenameTrack(ti, name) => {
                    self.multi_edit.ui.renaming_track = None;
                    let name = name.trim().to_string();
                    if name.is_empty() {
                        continue;
                    }
                    self.multi_edit_checkpoint();
                    if let Some(track) = self
                        .multi_edit_active_doc_mut()
                        .and_then(|doc| doc.tracks.get_mut(ti))
                    {
                        track.name = name;
                    }
                    self.multi_edit_mark_changed(&doc_id);
                }
                Action::SplitChannels(id) => {
                    self.multi_edit_split_clip_by_channel(&id);
                }
                Action::SetTrackOutput(track_id, output) => {
                    self.multi_edit_set_track_output(&track_id, output);
                }
                Action::AddLane(ti, param) => {
                    self.multi_edit_checkpoint();
                    if let Some(doc) = self.multi_edit_active_doc_mut() {
                        doc.ensure_lane(ti, param);
                        if let Some(track) = doc.tracks.get_mut(ti) {
                            track.lanes_collapsed = false;
                        }
                    }
                    self.multi_edit_touched();
                }
                Action::RemoveLane(ti, lane) => {
                    self.multi_edit_checkpoint();
                    if let Some(doc) = self.multi_edit_active_doc_mut() {
                        doc.remove_lane(ti, &lane);
                    }
                    self.multi_edit_touched();
                }
                Action::AddTrack(kind) => {
                    self.multi_edit_checkpoint();
                    if let Some(doc) = self.multi_edit_active_doc_mut() {
                        doc.add_track(kind);
                    }
                    self.multi_edit_touched();
                }
                Action::RemoveTrack(ti) => {
                    self.multi_edit_checkpoint();
                    if let Some(doc) = self.multi_edit_active_doc_mut() {
                        if let Some(id) = doc.tracks.get(ti).map(|t| t.id.clone()) {
                            doc.remove_track(&id);
                        }
                    }
                    self.multi_edit.ui.selected_track = None;
                    self.multi_edit_touched();
                }
                Action::InsertPoint {
                    track,
                    lane,
                    secs,
                    value,
                } => {
                    self.multi_edit_checkpoint();
                    if let Some(lane) = self
                        .multi_edit_active_doc_mut()
                        .and_then(|doc| doc.tracks.get_mut(track))
                        .and_then(|track| track.lanes.iter_mut().find(|l| l.id == lane))
                    {
                        lane.insert_point(secs, value);
                    }
                    self.multi_edit_touched();
                }
                Action::BeginPointDrag(drag) => {
                    self.multi_edit_checkpoint();
                    self.multi_edit.ui.lane_drag = Some(drag);
                }
                Action::OpenPointEditor(editor) => {
                    self.multi_edit_checkpoint();
                    self.multi_edit.ui.lane_drag = None;
                    self.multi_edit.ui.point_editor = Some(editor);
                    self.multi_edit.ui.point_editor_just_opened = true;
                }
                Action::BeginResize(resize) => {
                    self.multi_edit.ui.row_resize = Some(resize);
                }
                Action::AddMarker => {
                    self.multi_edit_add_marker_at_playhead();
                }
                Action::BeginMarkerDrag(id) => {
                    self.multi_edit_checkpoint();
                    self.multi_edit.ui.marker_drag = Some(id);
                }
                Action::BeginRenameMarker(id) => {
                    let label = self
                        .multi_edit_active_doc()
                        .and_then(|doc| doc.markers.iter().find(|m| m.id == id))
                        .map(|m| m.label.clone())
                        .unwrap_or_default();
                    self.multi_edit.ui.renaming_marker = Some((id, label));
                    self.multi_edit.ui.rename_focus_pending = true;
                }
                Action::RemoveMarker(id) => {
                    self.multi_edit_checkpoint();
                    if let Some(doc) = self.multi_edit_active_doc_mut() {
                        doc.remove_marker(&id);
                    }
                    self.multi_edit_mark_changed(&doc_id);
                }
                Action::Zoom {
                    factor,
                    anchor_secs,
                    anchor_x,
                } => {
                    let width = map.width();
                    if let Some(doc) = self.multi_edit_active_doc_mut() {
                        let pps = (doc.view.px_per_sec * factor)
                            .clamp(zoom_out_limit(doc.end_secs(), width), MAX_PX_PER_SEC);
                        doc.view.px_per_sec = pps;
                        let visible = (width / pps) as f64;
                        doc.view.scroll_secs = clamp_scroll_secs(
                            anchor_secs - (anchor_x / pps) as f64,
                            doc.end_secs(),
                            visible,
                        );
                    }
                }
                Action::ZoomVertical(factor) => self.multi_edit_zoom_rows(factor),
                Action::Scroll(delta) => {
                    let width = map.width();
                    if let Some(doc) = self.multi_edit_active_doc_mut() {
                        let visible = (width / doc.view.px_per_sec) as f64;
                        doc.view.scroll_secs =
                            clamp_scroll_secs(doc.view.scroll_secs + delta, doc.end_secs(), visible);
                    }
                }
                Action::ScrollYBy(delta) => {
                    let max_y = self.multi_edit.ui.max_scroll_y;
                    if let Some(doc) = self.multi_edit_active_doc_mut() {
                        doc.view.scroll_y = (doc.view.scroll_y + delta).clamp(0.0, max_y);
                    }
                }
                Action::ScrollYTo(offset) => {
                    let max_y = self.multi_edit.ui.max_scroll_y;
                    if let Some(doc) = self.multi_edit_active_doc_mut() {
                        doc.view.scroll_y = offset.clamp(0.0, max_y);
                    }
                }
            }
        }
    }

    fn multi_edit_toggle_track_flag(&mut self, ti: usize, mute: bool) {
        self.multi_edit_checkpoint();
        if let Some(track) = self
            .multi_edit_active_doc_mut()
            .and_then(|doc| doc.tracks.get_mut(ti))
        {
            if mute {
                track.mute = !track.mute;
            } else {
                track.solo = !track.solo;
            }
        }
        self.multi_edit_touched();
    }

    /// Carry a clip, point, marker or row-height drag with the pointer; end
    /// it on release. Every time one sets is snapped (Alt: not).
    fn multi_edit_continue_drags(
        &mut self,
        ctx: &egui::Context,
        doc: &MultiEditDoc,
        map: TimeMap,
        snap: &Snap,
        track_rows: &[(usize, Rect)],
        lane_rects: &HashMap<(String, String), Rect>,
    ) {
        let (down, pos) = ctx.input(|i| (i.pointer.primary_down(), i.pointer.interact_pos()));
        if !down {
            // A selection is no edit: the rectangle just goes.
            self.multi_edit.ui.marquee = None;
            let ended = self.multi_edit.ui.clip_drag.take().is_some()
                | self.multi_edit.ui.lane_drag.take().is_some()
                | self.multi_edit.ui.marker_drag.take().is_some();
            if self.multi_edit.ui.row_resize.take().is_some() {
                self.multi_edit_mark_changed(&doc.id);
            }
            if ended {
                self.multi_edit_touched();
            }
            return;
        }
        let Some(pos) = pos else {
            return;
        };
        let pointer_secs = map.secs(pos.x);
        if let Some((origin, additive)) = self.multi_edit.ui.marquee.as_ref().map(|m| (m.origin, m.additive)) {
            // Every clip the rectangle meets, on every track row it spans.
            let rect = Rect::from_two_pos(origin, pos);
            let (t0, t1) = (map.secs(rect.left().max(map.left)), map.secs(rect.right().max(map.left)));
            let tracks: Vec<usize> = track_rows
                .iter()
                .filter(|(_, row)| row.top() < rect.bottom() && row.bottom() > rect.top())
                .map(|(ti, _)| *ti)
                .collect();
            let mut ids: HashSet<String> = doc.clips_in_range(&tracks, t0, t1).into_iter().collect();
            if additive {
                if let Some(marquee) = self.multi_edit.ui.marquee.as_ref() {
                    ids.extend(marquee.base.iter().cloned());
                }
            }
            if ids != self.multi_edit.ui.selected_clips {
                self.multi_edit_select_clips(ids, None);
            }
            return;
        }
        if let Some(drag) = self.multi_edit.ui.clip_drag.clone() {
            ctx.set_cursor_icon(match drag.kind {
                ClipDragKind::Move { .. } => egui::CursorIcon::Grabbing,
                _ => egui::CursorIcon::ResizeHorizontal,
            });
            let Some(clip) = doc.clip(&drag.clip_id).cloned() else {
                self.multi_edit.ui.clip_drag = None;
                return;
            };
            // The clip's own edges are no target for itself.
            let own = [clip.start_secs, clip.end_secs()];
            let target = track_rows
                .iter()
                .find(|(_, rect)| pos.y >= rect.top() && pos.y < rect.bottom())
                .map(|(ti, _)| *ti);
            let Some(live) = self.multi_edit_active_doc_mut() else {
                return;
            };
            match drag.kind {
                ClipDragKind::Move { grab_secs } => {
                    // The grabbed clip goes where the pointer (and snapping)
                    // says; the rest of the group by as much. No clip of the
                    // group is a snap target for it.
                    let group_edges: Vec<f64> = drag
                        .group
                        .iter()
                        .filter_map(|(id, _, _)| doc.clip(id))
                        .flat_map(|c| [c.start_secs, c.end_secs()])
                        .chain(own)
                        .collect();
                    let raw_start = (pointer_secs - grab_secs).max(0.0);
                    let start = snap.apply_span(raw_start, clip.len_secs, &group_edges);
                    let origin = drag
                        .group
                        .iter()
                        .find(|(id, _, _)| *id == drag.clip_id)
                        .map(|(_, start, ti)| (*start, *ti));
                    match origin {
                        Some((origin_secs, origin_track)) => {
                            let rows = target.map_or(0, |ti| ti as i32 - origin_track as i32);
                            live.move_group(&drag.group, start - origin_secs, rows);
                        }
                        None => {
                            if !live.move_clip(&drag.clip_id, start.max(0.0), target) {
                                live.move_clip(&drag.clip_id, start.max(0.0), None);
                            }
                        }
                    }
                }
                ClipDragKind::TrimStart => {
                    live.trim_clip_start(&drag.clip_id, snap.apply(pointer_secs, &own));
                }
                ClipDragKind::TrimEnd => {
                    live.trim_clip_end(&drag.clip_id, snap.apply(pointer_secs, &own));
                }
                ClipDragKind::FadeIn => {
                    live.set_fade_in(&drag.clip_id, snap.apply(pointer_secs, &own) - clip.start_secs);
                }
                ClipDragKind::FadeOut => {
                    live.set_fade_out(&drag.clip_id, clip.end_secs() - snap.apply(pointer_secs, &own));
                }
            }
            self.multi_edit_touched();
        } else if let Some(drag) = self.multi_edit.ui.lane_drag.clone() {
            let Some(rect) = lane_rects.get(&(drag.track_id.clone(), drag.lane_id.clone())).copied() else {
                return;
            };
            let own = doc
                .tracks
                .iter()
                .find(|t| t.id == drag.track_id)
                .and_then(|t| t.lanes.iter().find(|l| l.id == drag.lane_id))
                .and_then(|l| l.points.get(drag.point))
                .map(|p| vec![p.secs])
                .unwrap_or_default();
            let secs = snap.apply(pointer_secs, &own);
            let Some(lane) = self
                .multi_edit_active_doc_mut()
                .and_then(|d| d.tracks.iter_mut().find(|t| t.id == drag.track_id))
                .and_then(|t| t.lanes.iter_mut().find(|l| l.id == drag.lane_id))
            else {
                return;
            };
            let (lo, hi) = lane.param.range();
            let value = lo + (rect.bottom() - pos.y) / rect.height().max(1.0) * (hi - lo);
            lane.move_point(drag.point, secs, value);
            self.multi_edit_touched();
        } else if let Some(id) = self.multi_edit.ui.marker_drag.clone() {
            let own: Vec<f64> = doc.markers.iter().filter(|m| m.id == id).map(|m| m.secs).collect();
            let secs = snap.apply(pointer_secs, &own);
            if let Some(live) = self.multi_edit_active_doc_mut() {
                live.move_marker(&id, secs);
            }
            self.multi_edit_mark_changed(&doc.id);
        } else if let Some(resize) = self.multi_edit.ui.row_resize.clone() {
            ctx.set_cursor_icon(egui::CursorIcon::ResizeVertical);
            let zoom = doc.view.track_zoom.max(MIN_TRACK_ZOOM);
            if let Some(live) = self.multi_edit_active_doc_mut() {
                match resize {
                    RowResize::Track { track_id, start_height, start_y } => {
                        if let Some(track) = live.tracks.iter_mut().find(|t| t.id == track_id) {
                            track.height = (start_height + (pos.y - start_y) / zoom)
                                .clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT);
                        }
                    }
                    RowResize::Lane { track_id, lane_id, start_height, start_y } => {
                        if let Some(lane) = live
                            .tracks
                            .iter_mut()
                            .find(|t| t.id == track_id)
                            .and_then(|t| t.lanes.iter_mut().find(|l| l.id == lane_id))
                        {
                            lane.height = (start_height + (pos.y - start_y) / zoom)
                                .clamp(MIN_LANE_HEIGHT, MAX_LANE_HEIGHT);
                        }
                    }
                }
            }
        }
    }

    fn ui_multi_edit_dialogs(&mut self, ctx: &egui::Context) {
        let Some(id) = self.multi_edit.ui.confirm_delete_doc.clone() else {
            return;
        };
        let name = self
            .multi_edit
            .doc(&id)
            .map(|doc| doc.name.clone())
            .unwrap_or_default();
        let mut close = false;
        egui::Modal::new(egui::Id::new("multi_edit_delete_confirm")).show(ctx, |ui| {
            ui.heading("Delete timeline");
            ui.label(format!(
                "Delete \"{name}\" and its tracks? The list's files are not touched."
            ));
            ui.horizontal(|ui| {
                if ui.button("Delete").clicked() {
                    self.multi_edit_delete(&id);
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.multi_edit.ui.confirm_delete_doc = None;
        }
    }
}

/// "m:ss.mmm" or plain seconds back into seconds, for the point popup.
fn parse_secs(text: &str) -> Option<f64> {
    let text = text.trim();
    match text.split_once(':') {
        Some((m, s)) => Some(m.trim().parse::<f64>().ok()? * 60.0 + s.trim().parse::<f64>().ok()?),
        None => text.parse::<f64>().ok(),
    }
}

/// How loud a clip is at timeline second `secs` from its fades alone, for
/// drawing the waveform the way it will sound.
fn fade_gain_at(clip: &Clip, secs: f64) -> f32 {
    let from_start = secs - clip.start_secs;
    let to_end = clip.end_secs() - secs;
    let mut gain = 1.0f64;
    if clip.fade_in_secs > 0.0 && from_start < clip.fade_in_secs {
        gain *= (from_start / clip.fade_in_secs).clamp(0.0, 1.0);
    }
    if clip.fade_out_secs > 0.0 && to_end < clip.fade_out_secs {
        gain *= (to_end / clip.fade_out_secs).clamp(0.0, 1.0);
    }
    gain as f32
}

#[cfg(test)]
mod tests {
    use super::parse_secs;

    #[test]
    fn typed_times_read_both_ways() {
        assert_eq!(parse_secs("1:02.500"), Some(62.5));
        assert_eq!(parse_secs(" 3.25 "), Some(3.25));
        assert_eq!(parse_secs("x"), None);
    }
}
