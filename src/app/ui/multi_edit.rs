//! The Multi Edits workspace: the list pane on the left, the timeline on the
//! right. What each part does is in `docs/MULTI_EDITS_SPEC.md`.
//!
//! The timeline is drawn from a copy of the document taken at the top of the
//! frame; everything the user does while it is drawn is collected as an
//! [`Action`] and applied afterwards. Drawing never holds the document while
//! editing it, and one frame's edits land together.

use std::collections::HashMap;
use std::path::PathBuf;

use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};

use crate::app::input_focus::UiSurface;
use crate::app::multi_edit::{
    Clip, LaneParam, MultiEditDoc, TrackKind, MAX_PX_PER_SEC, MIN_PX_PER_SEC, TRACK_GAIN_MAX_DB,
    TRACK_GAIN_MIN_DB,
};
use crate::app::multi_edit_ops::{ClipDrag, ClipDragKind, LaneDrag, SourceSlot};
use crate::app::types::{ColumnId, ListViewProfile};
use crate::app::WavesPreviewer;

/// Rows dragged from the list pane onto the timeline.
pub(crate) struct MultiEditRowDrag(pub Vec<PathBuf>);

/// Width of the track header column (name, fader, M/S).
const HEADER_W: f32 = 150.0;
const TRACK_H: f32 = 72.0;
const LANE_H: f32 = 56.0;
const RULER_H: f32 = 22.0;
/// The row under the tracks that holds the add button and takes drops that
/// should make a new track.
const ADD_ROW_H: f32 = 64.0;
/// How far inside a clip's edge a press trims instead of moving.
const EDGE_GRAB_PX: f32 = 6.0;
/// Side of the square handle at a clip's top corner that sets its fade.
const FADE_HANDLE_PX: f32 = 9.0;
/// How close, on screen, a dragged edge must come to another to snap to it.
const SNAP_PX: f32 = 8.0;
/// How far an automation lane is indented under its track, as in the design.
const LANE_INDENT: f32 = 14.0;
const POINT_RADIUS: f32 = 4.0;
/// How close a press must land to a point to grab it rather than add one.
const POINT_GRAB_PX: f32 = 7.0;
const CLIP_CORNER: f32 = 6.0;
/// Ruler ticks never crowd closer than this.
const RULER_MIN_TICK_PX: f32 = 70.0;
/// Zoom per notch of Ctrl+wheel is what egui reports; this is the button step.
const ZOOM_BUTTON_STEP: f32 = 1.5;

const AUDIO_CLIP_FILL: Color32 = Color32::from_rgb(46, 92, 140);
const VIDEO_CLIP_FILL: Color32 = Color32::from_rgb(104, 70, 150);
const MISSING_CLIP_FILL: Color32 = Color32::from_rgb(110, 50, 50);
const WAVE_COLOR: Color32 = Color32::from_rgb(190, 215, 240);
const PLAYHEAD_COLOR: Color32 = Color32::from_rgb(235, 80, 60);
/// The video preview's opening size (16:9) and the least it shrinks to.
const VIDEO_WINDOW_W: f32 = 360.0;
const VIDEO_WINDOW_H: f32 = 203.0;
const VIDEO_WINDOW_MIN_W: f32 = 160.0;
/// Gap between the preview's first position and the timeline's corner,
/// with room for the window's title bar.
const VIDEO_WINDOW_MARGIN: f32 = 40.0;
const LANE_LINE_COLOR: Color32 = Color32::from_rgb(240, 190, 90);

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
}

/// Everything a frame of the timeline asked for, applied after drawing.
enum Action {
    SelectClip(Option<String>),
    BeginClipDrag(ClipDrag),
    ClipCommand(String, ClipCommand),
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
    BeginRenameTrack(usize),
    CommitRenameTrack(usize, String),
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
    RemovePoint {
        track: usize,
        lane: String,
        point: usize,
    },
    Zoom {
        factor: f32,
        anchor_secs: f64,
        anchor_x: f32,
    },
    Scroll(f64),
}

#[derive(Clone, Copy)]
enum ClipCommand {
    SplitAtPlayhead,
    Duplicate,
    Delete,
}

/// "m:ss.mmm", the way the ruler and the readout show time.
fn format_secs(secs: f64) -> String {
    let secs = secs.max(0.0);
    let minutes = (secs / 60.0).floor() as u64;
    let rest = secs - minutes as f64 * 60.0;
    format!("{minutes}:{rest:06.3}")
}

/// The ruler's step: the smallest of these that keeps labels apart.
fn ruler_step(px_per_sec: f32) -> f64 {
    const STEPS: [f64; 14] = [
        0.01, 0.02, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 300.0, 600.0,
    ];
    STEPS
        .iter()
        .copied()
        .find(|step| (*step as f32) * px_per_sec >= RULER_MIN_TICK_PX)
        .unwrap_or(3600.0)
}

impl WavesPreviewer {
    /// One tab label per open timeline, in the workspace tab bar.
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
                let text = if active {
                    RichText::new(format!("[{name}]")).strong()
                } else {
                    RichText::new(name.clone())
                };
                let resp = ui.selectable_label(active, text);
                if resp.clicked() && !active {
                    self.multi_edit_open(&id);
                }
                resp.context_menu(|ui| {
                    if ui.button("Rename...").clicked() {
                        self.multi_edit.ui.renaming_doc = Some((id.clone(), name.clone()));
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
        egui::Panel::left("multi_edit_list_pane")
            .resizable(true)
            .default_size(320.0)
            .size_range(egui::Rangef::new(200.0, 720.0))
            .show_inside(ui, |ui| {
                self.ui_multi_edit_list_pane(ui, ctx);
            });
        let timeline = egui::CentralPanel::default()
            .show_inside(ui, |ui| {
                self.ui_multi_edit_timeline(ui, ctx);
            })
            .response
            .rect;
        self.ui_multi_edit_video_window(ctx, timeline);
        self.ui_multi_edit_dialogs(ctx);
    }

    /// The video preview: a small window showing the picture of the topmost
    /// video track at the playhead. Shown while the timeline has video.
    fn ui_multi_edit_video_window(&mut self, ctx: &egui::Context, timeline: Rect) {
        let Some(doc) = self.multi_edit_active_doc() else {
            return;
        };
        let has_video = doc
            .tracks
            .iter()
            .any(|track| track.kind == TrackKind::Video && !track.clips.is_empty());
        if !has_video {
            self.multi_edit.video_panels.clear();
            return;
        }
        let doc_id = doc.id.clone();
        let target = self.multi_edit_video_at_playhead();
        let playing = self.multi_edit_is_playing(&doc_id);
        if target.is_none() {
            self.multi_edit.video_panels.clear();
        }
        egui::Window::new("Video")
            .id(egui::Id::new(("multi_edit_video", doc_id.as_str())))
            .default_size([VIDEO_WINDOW_W, VIDEO_WINDOW_H])
            // First shown in the timeline's bottom-right corner, clear of the
            // list pane and the tab bar; after that, wherever it was left.
            .default_pos(
                timeline.right_bottom()
                    - Vec2::new(VIDEO_WINDOW_W, VIDEO_WINDOW_H)
                    - Vec2::splat(VIDEO_WINDOW_MARGIN),
            )
            .resizable(true)
            .collapsible(true)
            .show(ctx, |ui| {
                let size = ui
                    .available_size()
                    .max(Vec2::new(VIDEO_WINDOW_MIN_W, VIDEO_WINDOW_MIN_W * 9.0 / 16.0));
                let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
                match target.as_ref() {
                    Some((path, secs)) => {
                        let ppp = ctx.pixels_per_point();
                        let video = self.multi_edit_video_panel(path);
                        let id = video.id;
                        let aspect = video.panel.info.aspect();
                        video.panel.wanted_box_px = crate::app::render::video_panel::frame_box_px(
                            rect.shrink(2.0),
                            aspect,
                            ppp,
                        );
                        Self::paint_video_surface(
                            ui,
                            id,
                            &mut video.panel,
                            rect,
                            *secs,
                            true,
                            "multi_edit_video",
                        );
                    }
                    None => {
                        ui.painter().rect_filled(rect, 4.0, Color32::from_gray(16));
                        ui.painter().text(
                            rect.center(),
                            Align2::CENTER_CENTER,
                            "No video at the playhead",
                            FontId::proportional(12.0),
                            ui.visuals().weak_text_color(),
                        );
                    }
                }
            });
        if let Some((path, secs)) = target {
            self.multi_edit_request_video(&path, secs, playing);
        }
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

    fn ui_multi_edit_timeline(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(doc) = self.multi_edit_active_doc().cloned() else {
            return;
        };
        let mut actions: Vec<Action> = Vec::new();
        self.ui_multi_edit_header(ui, &doc);
        ui.separator();

        let area = ui.available_rect_before_wrap();
        self.ui_input_focus
            .register_region(UiSurface::MultiEdit, ui.layer_id(), area);
        let _scroll = self.pointer_scroll_input_guard(UiSurface::MultiEdit, ctx);
        let map = TimeMap {
            left: area.left() + HEADER_W,
            right: area.right(),
            px_per_sec: doc.view.px_per_sec.clamp(MIN_PX_PER_SEC, MAX_PX_PER_SEC),
            scroll_secs: doc.view.scroll_secs.max(0.0),
        };
        let playhead = self.multi_edit_playhead(&doc.id);

        // Ruler.
        let (ruler_row, _) = ui.allocate_exact_size(Vec2::new(area.width(), RULER_H), Sense::hover());
        let ruler = Rect::from_min_max(Pos2::new(map.left, ruler_row.top()), ruler_row.max);
        self.paint_ruler(ui, ruler, map);
        let ruler_resp = ui.interact(ruler, ui.id().with("multi_edit_ruler"), Sense::click_and_drag());
        if ruler_resp.clicked() || ruler_resp.dragged() {
            if let Some(pos) = ruler_resp.interact_pointer_pos() {
                actions.push(Action::Seek(map.secs(pos.x)));
            }
        }

        // Wheel over the lanes: Ctrl zooms about the pointer, Shift (or a
        // sideways swipe) scrolls time. The vertical wheel scrolls tracks.
        let body = Rect::from_min_max(Pos2::new(area.left(), ruler_row.bottom()), area.max);
        let hover = ctx.input(|i| i.pointer.hover_pos());
        if hover.is_some_and(|pos| pos.x >= map.left && body.contains(pos)) {
            let (zoom, dx) = ctx.input(|i| (i.zoom_delta(), i.smooth_scroll_delta.x));
            if (zoom - 1.0).abs() > f32::EPSILON {
                let x = hover.map(|p| p.x).unwrap_or(map.left);
                actions.push(Action::Zoom {
                    factor: zoom,
                    anchor_secs: map.secs(x),
                    anchor_x: x - map.left,
                });
            }
            if dx.abs() > 0.0 {
                actions.push(Action::Scroll(-(dx / map.px_per_sec) as f64));
                ctx.input_mut(|i| i.smooth_scroll_delta.x = 0.0);
            }
        }

        let mut track_rows: Vec<(usize, Rect)> = Vec::new();
        let mut lane_rects: HashMap<(String, String), Rect> = HashMap::new();
        let scroll_source = self.scroll_source_for(UiSurface::MultiEdit);
        egui::ScrollArea::vertical()
            .id_salt(("multi_edit_tracks", doc.id.as_str()))
            .auto_shrink([false, false])
            .scroll_source(scroll_source)
            .show(ui, |ui| {
                for (ti, track) in doc.tracks.iter().enumerate() {
                    let row = self.ui_multi_edit_track_row(ui, &doc, ti, map, &mut actions);
                    track_rows.push((ti, row));
                    for lane in &track.lanes {
                        let rect =
                            self.ui_multi_edit_lane_row(ui, &doc, ti, lane, map, &mut actions);
                        lane_rects.insert((track.id.clone(), lane.id.clone()), rect);
                    }
                }
                self.ui_multi_edit_add_row(ui, map, &mut actions);
            });

        // Playhead over everything below the header.
        let px = map.x(playhead);
        if px >= map.left && px <= map.right {
            ui.painter_at(area).line_segment(
                [Pos2::new(px, ruler.top()), Pos2::new(px, area.bottom())],
                Stroke::new(1.5, PLAYHEAD_COLOR),
            );
        }

        self.multi_edit_timeline_keys(ctx, &doc, playhead, &mut actions);
        self.apply_multi_edit_actions(actions);
        self.multi_edit_continue_drags(ctx, map, &track_rows, &lane_rects);
    }

    fn ui_multi_edit_header(&mut self, ui: &mut egui::Ui, doc: &MultiEditDoc) {
        ui.horizontal(|ui| {
            let renaming = self
                .multi_edit
                .ui
                .renaming_doc
                .as_ref()
                .is_some_and(|(id, _)| id == &doc.id);
            if renaming {
                let mut done = false;
                if let Some((_, text)) = self.multi_edit.ui.renaming_doc.as_mut() {
                    let resp = ui.add(egui::TextEdit::singleline(text).desired_width(200.0));
                    resp.request_focus();
                    done = resp.lost_focus();
                }
                if done {
                    if let Some((id, text)) = self.multi_edit.ui.renaming_doc.take() {
                        let name = text.trim().to_string();
                        if !name.is_empty() {
                            if let Some(doc) = self.multi_edit.doc_mut(&id) {
                                doc.name = name;
                            }
                        }
                    }
                }
            } else {
                let resp = ui
                    .add(egui::Label::new(RichText::new(&doc.name).strong()).sense(Sense::click()))
                    .on_hover_text("Double-click to rename");
                if resp.double_clicked() {
                    self.multi_edit.ui.renaming_doc = Some((doc.id.clone(), doc.name.clone()));
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
            if ui.small_button("\u{2212}").on_hover_text("Zoom out").clicked() {
                self.multi_edit_zoom_by(1.0 / ZOOM_BUTTON_STEP);
            }
            if ui.small_button("+").on_hover_text("Zoom in (Ctrl+wheel)").clicked() {
                self.multi_edit_zoom_by(ZOOM_BUTTON_STEP);
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
                        let can = self.multi_edit.export.is_none() && doc.end_secs() > 0.0;
                        if ui
                            .add_enabled(can, egui::Button::new("Export"))
                            .on_hover_text(
                                "Mix the timeline down into a new (virtual) row in the list. \
                                 Save it from there. Video tracks contribute their sound only.",
                            )
                            .clicked()
                        {
                            self.multi_edit_start_export();
                        }
                    }
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
        if let Some(doc) = self.multi_edit_active_doc_mut() {
            let before = doc.view.px_per_sec;
            let after = (before * factor).clamp(MIN_PX_PER_SEC, MAX_PX_PER_SEC);
            // Keep the playhead where it is on screen.
            let offset = (playhead - doc.view.scroll_secs) * before as f64;
            doc.view.px_per_sec = after;
            doc.view.scroll_secs = (playhead - offset / after as f64).max(0.0);
        }
    }

    fn paint_ruler(&self, ui: &egui::Ui, ruler: Rect, map: TimeMap) {
        let painter = ui.painter_at(ruler);
        let visuals = ui.visuals();
        painter.rect_filled(ruler, 0.0, visuals.extreme_bg_color);
        let step = ruler_step(map.px_per_sec);
        let (start, end) = map.visible_secs();
        let mut t = (start / step).floor() * step;
        while t <= end {
            let x = map.x(t);
            painter.line_segment(
                [Pos2::new(x, ruler.bottom() - 6.0), Pos2::new(x, ruler.bottom())],
                Stroke::new(1.0, visuals.weak_text_color()),
            );
            painter.text(
                Pos2::new(x + 3.0, ruler.center().y - 2.0),
                Align2::LEFT_CENTER,
                format_secs(t),
                FontId::monospace(10.0),
                visuals.weak_text_color(),
            );
            t += step;
        }
    }

    /// One track: its header and its clip lane. Returns the row's rect.
    fn ui_multi_edit_track_row(
        &mut self,
        ui: &mut egui::Ui,
        doc: &MultiEditDoc,
        ti: usize,
        map: TimeMap,
        actions: &mut Vec<Action>,
    ) -> Rect {
        let track = &doc.tracks[ti];
        let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), TRACK_H), Sense::hover());
        let header = Rect::from_min_max(row.min, Pos2::new(row.left() + HEADER_W, row.bottom()));
        let lane = Rect::from_min_max(Pos2::new(map.left, row.top()), row.max);
        let visuals = ui.visuals().clone();
        ui.painter().rect_filled(header, 0.0, visuals.faint_bg_color);
        ui.painter()
            .rect_filled(lane, 0.0, visuals.extreme_bg_color.gamma_multiply(0.6));
        ui.painter().line_segment(
            [row.left_bottom(), row.right_bottom()],
            Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
        );
        ui.painter().line_segment(
            [header.right_top(), header.right_bottom()],
            Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
        );

        // Header: name, fader, M / S, and the track menu.
        let header_resp = ui.interact(header, ui.id().with(("me_track_hdr", &track.id)), Sense::click());
        header_resp.context_menu(|ui| {
            ui.menu_button("Add lane", |ui| {
                for param in LaneParam::ALL {
                    let exists = track.lane(param).is_some();
                    if ui.add_enabled(!exists, egui::Button::new(param.label())).clicked() {
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
        ui.scope_builder(egui::UiBuilder::new().max_rect(header.shrink(6.0)), |ui| {
            let renaming = self
                .multi_edit
                .ui
                .renaming_track
                .as_ref()
                .is_some_and(|(id, _)| id == &track.id);
            if renaming {
                let mut commit = None;
                if let Some((_, text)) = self.multi_edit.ui.renaming_track.as_mut() {
                    let resp = ui.add(egui::TextEdit::singleline(text).desired_width(HEADER_W - 16.0));
                    resp.request_focus();
                    if resp.lost_focus() {
                        commit = Some(text.clone());
                    }
                }
                if let Some(name) = commit {
                    actions.push(Action::CommitRenameTrack(ti, name));
                }
            } else {
                let name = ui
                    .add(egui::Label::new(RichText::new(&track.name).strong()).sense(Sense::click()))
                    .on_hover_text("Double-click to rename; right-click for lanes");
                if name.double_clicked() {
                    actions.push(Action::BeginRenameTrack(ti));
                }
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().slider_width = HEADER_W - 70.0;
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
                if ui
                    .selectable_label(track.mute, "M")
                    .on_hover_text("Mute")
                    .clicked()
                {
                    actions.push(Action::ToggleMute(ti));
                }
                if ui
                    .selectable_label(track.solo, "S")
                    .on_hover_text("Solo")
                    .clicked()
                {
                    actions.push(Action::ToggleSolo(ti));
                }
            });
        });

        // The lane: a click on empty space seeks and deselects; rows dropped
        // here join this track.
        let lane_resp = ui.interact(lane, ui.id().with(("me_lane", &track.id)), Sense::click());
        if lane_resp.clicked() {
            if let Some(pos) = lane_resp.interact_pointer_pos() {
                actions.push(Action::SelectClip(None));
                actions.push(Action::Seek(map.secs(pos.x)));
            }
        }
        self.multi_edit_drop_target(ui, lane, Some(ti), map, actions);

        let painter = ui.painter_at(lane);
        for clip in &track.clips {
            self.ui_multi_edit_clip(ui, &painter, track.kind, clip, lane, map, actions);
        }
        row
    }

    /// Accept rows dragged from the list pane over `rect`, showing where they
    /// would land.
    ///
    /// Tested against the rect itself rather than a response: a drop onto a
    /// clip is a drop onto its track, and a response is "covered" wherever a
    /// clip sits on it.
    fn multi_edit_drop_target(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        track: Option<usize>,
        map: TimeMap,
        actions: &mut Vec<Action>,
    ) {
        let ctx = ui.ctx();
        if !egui::DragAndDrop::has_payload_of_type::<MultiEditRowDrag>(ctx) {
            return;
        }
        let Some(pos) = ctx.pointer_hover_pos().or_else(|| ctx.pointer_interact_pos()) else {
            return;
        };
        if !rect.contains(pos) || ctx.layer_id_at(pos) != Some(ui.layer_id()) {
            return;
        }
        let x = pos.x.max(map.left);
        let accent = ui.visuals().selection.bg_fill;
        ui.painter()
            .rect_stroke(rect, 0.0, Stroke::new(1.5, accent), StrokeKind::Inside);
        ui.painter().line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(2.0, accent),
        );
        if ctx.input(|i| i.pointer.any_released()) {
            if let Some(payload) = egui::DragAndDrop::take_payload::<MultiEditRowDrag>(ctx) {
                actions.push(Action::DropRows {
                    track,
                    at_secs: map.secs(x),
                    paths: payload.0.clone(),
                });
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
        lane: Rect,
        map: TimeMap,
        actions: &mut Vec<Action>,
    ) {
        let x0 = map.x(clip.start_secs);
        let x1 = map.x(clip.end_secs());
        if x1 < lane.left() || x0 > lane.right() {
            return;
        }
        let rect = Rect::from_min_max(Pos2::new(x0, lane.top() + 4.0), Pos2::new(x1.max(x0 + 2.0), lane.bottom() - 4.0));
        let selected = self.multi_edit.ui.selected_clip.as_deref() == Some(clip.id.as_str());
        let poster = (kind == TrackKind::Video)
            .then(|| {
                self.item_for_path(&clip.source.path)
                    .and_then(|item| item.meta.as_ref())
                    .and_then(|meta| meta.cover_art.clone())
            })
            .flatten()
            .map(|art| self.list_art_texture_for_path(ui.ctx(), &clip.source.path, art));
        let slot = self.multi_edit.sources.get(&clip.source.path);
        let (fill, status) = match slot {
            Some(SourceSlot::Failed(err)) => (MISSING_CLIP_FILL, Some(format!("missing: {err}"))),
            Some(SourceSlot::NotInList) => (MISSING_CLIP_FILL, Some("not in the list".to_string())),
            Some(SourceSlot::Loading) | None => (
                match kind {
                    TrackKind::Audio => AUDIO_CLIP_FILL,
                    TrackKind::Video => VIDEO_CLIP_FILL,
                },
                Some("reading...".to_string()),
            ),
            Some(SourceSlot::Ready(_)) => (
                match kind {
                    TrackKind::Audio => AUDIO_CLIP_FILL,
                    TrackKind::Video => VIDEO_CLIP_FILL,
                },
                None,
            ),
        };
        painter.rect_filled(rect, CLIP_CORNER, fill.gamma_multiply(0.85));
        // A video clip opens with its poster frame, when the list has one.
        let mut wave_left = rect.left();
        if kind == TrackKind::Video {
            if let Some(texture) = poster {
                let tex = texture.size_vec2().max(Vec2::splat(1.0));
                let h = rect.height() - 4.0;
                let w = (h * tex.x / tex.y).min(rect.width() * 0.5);
                let thumb = Rect::from_min_size(rect.min + Vec2::new(2.0, 2.0), Vec2::new(w, h));
                painter.image(
                    texture.id(),
                    thumb,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                wave_left = thumb.right() + 2.0;
            }
        }

        // Waveform of the part of the clip on screen.
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
                    .peaks
                    .query_columns(s0, s1.max(s0 + 1), width, 0.0, &mut peaks);
                let mid = rect.center().y;
                let half = rect.height() * 0.45;
                for (i, peak) in peaks.iter().enumerate() {
                    let x = vis_x0 + i as f32 + 0.5;
                    let gain = fade_gain_at(clip, map.secs(x));
                    let top = mid - peak.max.clamp(-1.0, 1.0) * half * gain;
                    let bottom = mid - peak.min.clamp(-1.0, 1.0) * half * gain;
                    painter.line_segment(
                        [Pos2::new(x, top), Pos2::new(x, bottom.max(top + 1.0))],
                        Stroke::new(1.0, WAVE_COLOR.gamma_multiply(0.8)),
                    );
                }
            }
        }

        // Fades, drawn as the design has them: a line from the bottom corner
        // up to where the fade ends.
        let fade_stroke = Stroke::new(1.2, Color32::WHITE.gamma_multiply(0.8));
        if clip.fade_in_secs > 0.0 {
            let fx = map.x(clip.start_secs + clip.fade_in_secs);
            painter.line_segment([rect.left_bottom(), Pos2::new(fx, rect.top())], fade_stroke);
        }
        if clip.fade_out_secs > 0.0 {
            let fx = map.x(clip.end_secs() - clip.fade_out_secs);
            painter.line_segment([Pos2::new(fx, rect.top()), rect.right_bottom()], fade_stroke);
        }
        let outline = if selected {
            Stroke::new(2.0, ui.visuals().selection.stroke.color)
        } else {
            Stroke::new(1.0, Color32::from_gray(200).gamma_multiply(0.6))
        };
        painter.rect_stroke(rect, CLIP_CORNER, outline, StrokeKind::Inside);
        let label = match &status {
            Some(status) => format!("{}  ({status})", clip.name),
            None => clip.name.clone(),
        };
        painter.text(
            Pos2::new(rect.left().max(lane.left()) + 6.0, rect.top() + 3.0),
            Align2::LEFT_TOP,
            label,
            FontId::proportional(11.0),
            Color32::WHITE,
        );

        // Interaction: body moves, edges trim, the top corners set fades.
        // Later rects win where they overlap, so the smallest go last.
        let id = ui.id().with(("me_clip", &clip.id));
        let body = ui.interact(rect, id, Sense::click_and_drag());
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
            let grab = body
                .interact_pointer_pos()
                .map(|pos| map.secs(pos.x) - clip.start_secs)
                .unwrap_or(0.0);
            actions.push(begin(ClipDragKind::Move { grab_secs: grab }));
        } else if body.clicked() {
            actions.push(Action::SelectClip(Some(clip.id.clone())));
        }
        if body.hovered() && self.multi_edit.ui.clip_drag.is_none() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        body.context_menu(|ui| {
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
        });
    }

    /// An automation lane under its track. Returns the lane's value rect.
    fn ui_multi_edit_lane_row(
        &mut self,
        ui: &mut egui::Ui,
        doc: &MultiEditDoc,
        ti: usize,
        lane: &crate::app::multi_edit::AutomationLane,
        map: TimeMap,
        actions: &mut Vec<Action>,
    ) -> Rect {
        let track = &doc.tracks[ti];
        let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), LANE_H), Sense::hover());
        let header = Rect::from_min_max(row.min, Pos2::new(row.left() + HEADER_W, row.bottom()));
        let area = Rect::from_min_max(Pos2::new(map.left, row.top()), row.max);
        let visuals = ui.visuals().clone();
        let line = visuals.widgets.noninteractive.bg_stroke.color;
        ui.painter().rect_filled(header, 0.0, visuals.faint_bg_color);
        ui.painter().rect_filled(area, 0.0, visuals.extreme_bg_color.gamma_multiply(0.35));
        // The bracket that ties the lane to its track, as in the design.
        let bracket_x = header.left() + LANE_INDENT * 0.5;
        ui.painter().line_segment(
            [Pos2::new(bracket_x, row.top()), Pos2::new(bracket_x, row.bottom())],
            Stroke::new(1.0, line),
        );
        ui.painter()
            .line_segment([row.left_bottom(), row.right_bottom()], Stroke::new(1.0, line));
        ui.painter().line_segment(
            [header.right_top(), header.right_bottom()],
            Stroke::new(1.0, line),
        );
        ui.painter().text(
            Pos2::new(header.left() + LANE_INDENT + 4.0, header.center().y),
            Align2::LEFT_CENTER,
            lane.param.label(),
            FontId::proportional(13.0),
            visuals.text_color(),
        );
        let remove = Rect::from_center_size(
            Pos2::new(header.right() - 14.0, header.center().y),
            Vec2::splat(16.0),
        );
        let remove_resp = ui
            .interact(remove, ui.id().with(("me_lane_rm", &lane.id)), Sense::click())
            .on_hover_text("Remove this lane");
        ui.painter().text(
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

        let inner = area.shrink2(Vec2::new(0.0, 5.0));
        let (lo, hi) = lane.param.range();
        let y_of = |v: f32| inner.bottom() - (v - lo) / (hi - lo) * inner.height();
        let value_of = |y: f32| lane.param.clamp(lo + (inner.bottom() - y) / inner.height() * (hi - lo));
        let painter = ui.painter_at(area);
        // The neutral value, for reference.
        let neutral_y = y_of(lane.param.neutral());
        painter.line_segment(
            [Pos2::new(area.left(), neutral_y), Pos2::new(area.right(), neutral_y)],
            Stroke::new(1.0, line.gamma_multiply(0.6)),
        );
        let stroke = Stroke::new(1.5, LANE_LINE_COLOR);
        let (vis0, vis1) = map.visible_secs();
        if lane.points.is_empty() {
            painter.line_segment(
                [Pos2::new(area.left(), neutral_y), Pos2::new(area.right(), neutral_y)],
                Stroke::new(1.0, LANE_LINE_COLOR.gamma_multiply(0.5)),
            );
        } else {
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
            painter.add(egui::Shape::line(pts, stroke));
            for point in &lane.points {
                painter.circle_filled(Pos2::new(map.x(point.secs), y_of(point.value)), POINT_RADIUS, LANE_LINE_COLOR);
            }
        }

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
        if let (Some(pos), true) = (resp.hover_pos(), resp.hovered()) {
            let text = match nearest {
                Some(i) => lane.param.format_value(lane.points[i].value),
                None => lane.param.format_value(value_of(pos.y)),
            };
            resp.clone().on_hover_text_at_pointer(text);
        }
        if resp.drag_started() {
            if let Some(point) = nearest {
                actions.push(Action::BeginPointDrag(LaneDrag {
                    track_id: track.id.clone(),
                    lane_id: lane.id.clone(),
                    point,
                }));
            }
        } else if resp.double_clicked() || resp.secondary_clicked() {
            if let Some(point) = nearest {
                actions.push(Action::RemovePoint {
                    track: ti,
                    lane: lane.id.clone(),
                    point,
                });
            }
        } else if resp.clicked() {
            if let (Some(pos), None) = (pointer, nearest) {
                actions.push(Action::InsertPoint {
                    track: ti,
                    lane: lane.id.clone(),
                    secs: map.secs(pos.x),
                    value: value_of(pos.y),
                });
            }
        }
        inner
    }

    /// The row under the tracks: the (+) button, and a drop zone that makes
    /// a new track.
    fn ui_multi_edit_add_row(&mut self, ui: &mut egui::Ui, map: TimeMap, actions: &mut Vec<Action>) {
        let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ADD_ROW_H), Sense::hover());
        let area = Rect::from_min_max(Pos2::new(map.left, row.top()), row.max);
        self.multi_edit_drop_target(ui, area, None, map, actions);
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
    }

    /// Delete / S / Ctrl+D on the selected clip, while the timeline owns
    /// the keys.
    fn multi_edit_timeline_keys(
        &mut self,
        ctx: &egui::Context,
        doc: &MultiEditDoc,
        _playhead: f64,
        actions: &mut Vec<Action>,
    ) {
        if !self.surface_keys_allowed(UiSurface::MultiEdit) {
            return;
        }
        let Some(clip) = self
            .multi_edit
            .ui
            .selected_clip
            .clone()
            .filter(|id| doc.clip(id).is_some())
        else {
            return;
        };
        let (delete, split, duplicate) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::Delete)
                    | i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace),
                i.consume_key(egui::Modifiers::NONE, egui::Key::S),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::D),
            )
        });
        if delete {
            actions.push(Action::ClipCommand(clip, ClipCommand::Delete));
        } else if split {
            actions.push(Action::ClipCommand(clip, ClipCommand::SplitAtPlayhead));
        } else if duplicate {
            actions.push(Action::ClipCommand(clip, ClipCommand::Duplicate));
        }
    }

    fn apply_multi_edit_actions(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::SelectClip(id) => self.multi_edit_select_clip(id),
                Action::BeginClipDrag(drag) => {
                    self.multi_edit_checkpoint();
                    self.multi_edit_select_clip(Some(drag.clip_id.clone()));
                    self.multi_edit.ui.clip_drag = Some(drag);
                }
                Action::ClipCommand(id, command) => {
                    let playhead = self
                        .multi_edit
                        .active
                        .as_deref()
                        .map(|doc| self.multi_edit_playhead(doc))
                        .unwrap_or(0.0);
                    self.multi_edit_checkpoint();
                    let Some(doc) = self.multi_edit_active_doc_mut() else {
                        continue;
                    };
                    let selected = match command {
                        ClipCommand::SplitAtPlayhead => doc.split_clip(&id, playhead).map(|_| id.clone()),
                        ClipCommand::Duplicate => doc.duplicate_clip(&id),
                        ClipCommand::Delete => {
                            doc.remove_clip(&id);
                            None
                        }
                    };
                    self.multi_edit.ui.selected_clip = selected;
                    self.multi_edit_touched();
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
                Action::BeginRenameTrack(ti) => {
                    if let Some(track) = self.multi_edit_active_doc().and_then(|doc| doc.tracks.get(ti)) {
                        self.multi_edit.ui.renaming_track = Some((track.id.clone(), track.name.clone()));
                    }
                }
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
                }
                Action::AddLane(ti, param) => {
                    self.multi_edit_checkpoint();
                    if let Some(doc) = self.multi_edit_active_doc_mut() {
                        doc.ensure_lane(ti, param);
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
                Action::RemovePoint { track, lane, point } => {
                    self.multi_edit_checkpoint();
                    if let Some(lane) = self
                        .multi_edit_active_doc_mut()
                        .and_then(|doc| doc.tracks.get_mut(track))
                        .and_then(|track| track.lanes.iter_mut().find(|l| l.id == lane))
                    {
                        lane.remove_point(point);
                    }
                    self.multi_edit_touched();
                }
                Action::Zoom {
                    factor,
                    anchor_secs,
                    anchor_x,
                } => {
                    if let Some(doc) = self.multi_edit_active_doc_mut() {
                        let pps = (doc.view.px_per_sec * factor).clamp(MIN_PX_PER_SEC, MAX_PX_PER_SEC);
                        doc.view.px_per_sec = pps;
                        doc.view.scroll_secs = (anchor_secs - (anchor_x / pps) as f64).max(0.0);
                    }
                }
                Action::Scroll(delta) => {
                    if let Some(doc) = self.multi_edit_active_doc_mut() {
                        doc.view.scroll_secs = (doc.view.scroll_secs + delta).max(0.0);
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

    /// Carry a clip or point drag with the pointer; end it on release.
    fn multi_edit_continue_drags(
        &mut self,
        ctx: &egui::Context,
        map: TimeMap,
        track_rows: &[(usize, Rect)],
        lane_rects: &HashMap<(String, String), Rect>,
    ) {
        let (down, pos, alt) = ctx.input(|i| {
            (
                i.pointer.primary_down(),
                i.pointer.interact_pos(),
                i.modifiers.alt,
            )
        });
        if !down {
            if self.multi_edit.ui.clip_drag.take().is_some()
                | self.multi_edit.ui.lane_drag.take().is_some()
            {
                self.multi_edit_touched();
            }
            return;
        }
        let Some(pos) = pos else {
            return;
        };
        let pointer_secs = map.secs(pos.x);
        if let Some(drag) = self.multi_edit.ui.clip_drag.clone() {
            ctx.set_cursor_icon(match drag.kind {
                ClipDragKind::Move { .. } => egui::CursorIcon::Grabbing,
                _ => egui::CursorIcon::ResizeHorizontal,
            });
            let playhead = self.multi_edit_playhead(self.multi_edit.active.as_deref().unwrap_or(""));
            let Some(doc) = self.multi_edit_active_doc_mut() else {
                return;
            };
            let snap_secs = (SNAP_PX / map.px_per_sec) as f64;
            let mut candidates = doc.snap_points(Some(&drag.clip_id));
            candidates.push(playhead);
            let snap = |t: f64| -> f64 {
                if alt {
                    return t;
                }
                candidates
                    .iter()
                    .copied()
                    .filter(|c| (c - t).abs() <= snap_secs)
                    .min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs()))
                    .unwrap_or(t)
            };
            let Some(clip) = doc.clip(&drag.clip_id).cloned() else {
                self.multi_edit.ui.clip_drag = None;
                return;
            };
            match drag.kind {
                ClipDragKind::Move { grab_secs } => {
                    let raw_start = (pointer_secs - grab_secs).max(0.0);
                    let snapped_start = snap(raw_start);
                    let snapped_end = snap(raw_start + clip.len_secs) - clip.len_secs;
                    let start = if (snapped_start - raw_start).abs() <= (snapped_end - raw_start).abs() {
                        snapped_start
                    } else {
                        snapped_end
                    };
                    let target = track_rows
                        .iter()
                        .find(|(_, rect)| pos.y >= rect.top() && pos.y < rect.bottom())
                        .map(|(ti, _)| *ti);
                    if !doc.move_clip(&drag.clip_id, start.max(0.0), target) {
                        doc.move_clip(&drag.clip_id, start.max(0.0), None);
                    }
                }
                ClipDragKind::TrimStart => {
                    doc.trim_clip_start(&drag.clip_id, snap(pointer_secs));
                }
                ClipDragKind::TrimEnd => {
                    doc.trim_clip_end(&drag.clip_id, snap(pointer_secs));
                }
                ClipDragKind::FadeIn => {
                    doc.set_fade_in(&drag.clip_id, pointer_secs - clip.start_secs);
                }
                ClipDragKind::FadeOut => {
                    doc.set_fade_out(&drag.clip_id, clip.end_secs() - pointer_secs);
                }
            }
            self.multi_edit_touched();
        } else if let Some(drag) = self.multi_edit.ui.lane_drag.clone() {
            let Some(rect) = lane_rects.get(&(drag.track_id.clone(), drag.lane_id.clone())).copied() else {
                return;
            };
            let Some(doc) = self.multi_edit_active_doc_mut() else {
                return;
            };
            let Some(lane) = doc
                .tracks
                .iter_mut()
                .find(|t| t.id == drag.track_id)
                .and_then(|t| t.lanes.iter_mut().find(|l| l.id == drag.lane_id))
            else {
                return;
            };
            let (lo, hi) = lane.param.range();
            let value = lo + (rect.bottom() - pos.y) / rect.height().max(1.0) * (hi - lo);
            lane.move_point(drag.point, pointer_secs, value);
            self.multi_edit_touched();
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
