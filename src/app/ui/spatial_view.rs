//! The Spatial view: where an object-audio file's beds and objects are
//! heard, and when.
//!
//! A full page in place of the waveform (like the Metadata view): the
//! element list on the left; the room seen from above and from the side,
//! with every element at the playhead and the selected one's path; and the
//! selected element's position over time as three keyframe lanes on a
//! timeline of their own. Dragging in the room writes a keyframe at the
//! playhead; dragging in a lane moves one. Edits go to the session
//! (`spatial_ops`), never into the file. See `docs/SPATIAL_AUDIO_SPEC.md`.

use std::path::PathBuf;
use std::sync::Arc;

use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};

use crate::app::spatial_ops::{SceneStatus, SpatialDrag};
use crate::app::types::EditorPrimaryView;
use crate::app::WavesPreviewer;
use crate::spatial::coords::speaker_cart;
use crate::spatial::panner::bed_layout;
use crate::spatial::scene::{
    from_cart, move_keyframe, put_keyframe, remove_keyframe, sample_keyframes, to_cart, Coords,
    Element, ElementKind, Keyframe, ObjectScene,
};

const LIST_W: f32 = 250.0;
const ROOM_MAX: f32 = 300.0;
const LANE_H: f32 = 70.0;
const RULER_H: f32 = 18.0;
const POINT_R: f32 = 4.0;
const PICK_PX: f32 = 9.0;
/// How far either side of the playhead the selected element's path is
/// drawn in the room views, in seconds.
const TRAIL_SECS: f64 = 2.0;
/// Points on that path.
const TRAIL_POINTS: usize = 48;
/// The narrowest timeline the zoom allows, in seconds.
const MIN_VIEW_SECS: f64 = 0.05;
/// Zoom per wheel notch.
const ZOOM_STEP: f64 = 1.25;

/// What one frame of the view asked for, applied after drawing.
enum Act {
    Leave(EditorPrimaryView),
    Select(Arc<str>),
    Mute(Arc<str>),
    Solo(Arc<str>),
    Seek(f64),
    /// Take the undo point for a gesture.
    Begin(SpatialDrag),
    End,
    /// The selected element to `cart`, at the playhead.
    Place([f32; 3]),
    Move {
        index: usize,
        secs: f64,
        pos: [f32; 3],
    },
    Insert {
        secs: f64,
        pos: [f32; 3],
    },
    Remove(usize),
    SelectKey(Option<usize>),
    Reset,
    View {
        start: f64,
        secs: f64,
    },
    Export,
    CancelExport,
}

/// A colour per element, the same every run.
fn element_color(key: &str) -> Color32 {
    let mut hash: u32 = 2_166_136_261;
    for byte in key.bytes() {
        hash = (hash ^ byte as u32).wrapping_mul(16_777_619);
    }
    let hue = (hash % 360) as f32 / 360.0;
    egui::ecolor::Hsva::new(hue, 0.62, 0.92, 1.0).into()
}

fn axis_labels(coords: Coords) -> [&'static str; 3] {
    match coords {
        Coords::Cartesian => ["X", "Y", "Z"],
        Coords::Polar => ["Azimuth", "Elevation", "Distance"],
    }
}

fn axis_range(coords: Coords, axis: usize) -> (f32, f32) {
    match (coords, axis) {
        (Coords::Cartesian, _) => (-1.0, 1.0),
        (Coords::Polar, 0) => (-180.0, 180.0),
        (Coords::Polar, 1) => (-90.0, 90.0),
        (Coords::Polar, _) => (0.0, 1.0),
    }
}

/// The room from above: X to the right, the front wall at the top.
struct TopView {
    rect: Rect,
}

impl TopView {
    fn to_screen(&self, cart: [f32; 3]) -> Pos2 {
        let c = self.rect.center();
        let half = self.rect.width() * 0.5 - 14.0;
        Pos2::new(
            c.x + cart[0].clamp(-1.0, 1.0) * half,
            c.y - cart[1].clamp(-1.0, 1.0) * half,
        )
    }

    fn from_screen(&self, pos: Pos2) -> (f32, f32) {
        let c = self.rect.center();
        let half = self.rect.width() * 0.5 - 14.0;
        (
            ((pos.x - c.x) / half).clamp(-1.0, 1.0),
            (-(pos.y - c.y) / half).clamp(-1.0, 1.0),
        )
    }
}

/// The room from the side: the front wall on the right, the ceiling at the
/// top, ear level at the middle line.
struct SideView {
    rect: Rect,
}

impl SideView {
    fn to_screen(&self, cart: [f32; 3]) -> Pos2 {
        let r = self.rect.shrink(14.0);
        let x = r.left() + (cart[1].clamp(-1.0, 1.0) + 1.0) * 0.5 * r.width();
        let y = r.bottom() - (cart[2].clamp(-1.0, 1.0) + 1.0) * 0.5 * r.height();
        Pos2::new(x, y)
    }

    fn from_screen(&self, pos: Pos2) -> (f32, f32) {
        let r = self.rect.shrink(14.0);
        (
            ((pos.x - r.left()) / r.width() * 2.0 - 1.0).clamp(-1.0, 1.0),
            ((r.bottom() - pos.y) / r.height() * 2.0 - 1.0).clamp(-1.0, 1.0),
        )
    }
}

/// The lanes' time axis.
#[derive(Clone, Copy)]
struct TimeAxis {
    left: f32,
    width: f32,
    start: f64,
    secs: f64,
}

impl TimeAxis {
    fn x(&self, secs: f64) -> f32 {
        self.left + ((secs - self.start) / self.secs) as f32 * self.width
    }

    fn secs(&self, x: f32) -> f64 {
        self.start + ((x - self.left) / self.width) as f64 * self.secs
    }
}

/// The keyframes an element plays: the session's edit, or the file's.
fn keys_of<'a>(
    element: &'a Element,
    edits: Option<&'a crate::spatial::scene::SceneEdits>,
) -> &'a Arc<[Keyframe]> {
    edits
        .map(|edits| edits.keyframes_for(element))
        .unwrap_or(&element.keyframes)
}

fn cart_of(element: &Element, keys: &[Keyframe], secs: f64) -> Option<[f32; 3]> {
    sample_keyframes(keys, element.coords, secs).map(|(pos, _)| to_cart(element.coords, pos))
}

/// A time ruler step that leaves room between labels.
fn ruler_step(secs_per_px: f64) -> f64 {
    let want = secs_per_px * 80.0;
    for step in [
        0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0,
    ] {
        if step >= want {
            return step;
        }
    }
    1200.0
}

fn format_secs(secs: f64) -> String {
    let secs = secs.max(0.0);
    let minutes = (secs / 60.0).floor();
    format!("{}:{:06.3}", minutes as u64, secs - minutes * 60.0)
}

impl WavesPreviewer {
    pub(in crate::app) fn ui_spatial_view(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        tab_idx: usize,
    ) {
        let path: PathBuf = self.tabs[tab_idx].path.clone();
        self.ensure_object_scene(&path);
        let status = self.object_scene_status(&path);
        let edits = self.spatial_edits_for(&path);
        let playing_here = matches!(
            &self.playback_session.source,
            crate::app::PlaybackSourceKind::EditorTab(p) if *p == path
        );
        let playhead = if playing_here {
            self.playback_current_source_time_sec().unwrap_or(0.0)
        } else {
            0.0
        };
        let is_playing = playing_here
            && self
                .audio
                .shared
                .playing
                .load(std::sync::atomic::Ordering::Relaxed);
        let monitor = self.spatial.monitor.get(&path).cloned().unwrap_or_default();
        let export_progress = self.spatial_export_progress();
        let mut acts: Vec<Act> = Vec::new();

        // ---- Toolbar ---------------------------------------------------
        let scene = match &status {
            SceneStatus::Ready(scene) => Some(Arc::clone(scene)),
            _ => None,
        };
        let state = self.tabs[tab_idx].spatial.clone();
        let selected = state
            .selected
            .clone()
            .and_then(|key| scene.as_ref().and_then(|s| s.element(&key).map(|_| key)))
            .or_else(|| {
                scene
                    .as_ref()
                    .and_then(|s| {
                        s.elements
                            .iter()
                            .find(|e| e.is_object())
                            .or(s.elements.first())
                    })
                    .map(|e| Arc::clone(&e.key))
            });
        let mut ramp = state.ramp_secs;
        ui.horizontal_wrapped(|ui| {
            ui.label("View:");
            egui::ComboBox::from_id_salt(("spatial_primary_view", self.tabs[tab_idx].tab_id))
                .selected_text("Spatial")
                .show_ui(ui, |ui| {
                    for (view, label) in [
                        (EditorPrimaryView::Wave, "Wave"),
                        (EditorPrimaryView::Spec, "Spec"),
                        (EditorPrimaryView::Other, "Other"),
                        (EditorPrimaryView::Metadata, "Metadata"),
                    ] {
                        if ui.selectable_label(false, label).clicked() {
                            acts.push(Act::Leave(view));
                        }
                    }
                    let _ = ui.selectable_label(true, "Spatial");
                });
            ui.separator();
            match &status {
                SceneStatus::Absent | SceneStatus::Loading(_) => {
                    ui.spinner();
                    let progress = match &status {
                        SceneStatus::Loading(p) => *p,
                        _ => 0.0,
                    };
                    ui.label(format!("Reading the scene\u{2026} {:.0}%", progress * 100.0));
                }
                SceneStatus::Failed(message) => {
                    ui.label(
                        RichText::new(format!("The scene could not be read: {message}"))
                            .color(ui.visuals().warn_fg_color),
                    );
                }
                SceneStatus::Ready(scene) => {
                    let mut line = format!(
                        "{} \u{b7} {} bed + {} obj",
                        scene.format.label(),
                        scene.bed_count(),
                        scene.object_count()
                    );
                    if let Some(programme) = &scene.programme {
                        line.push_str(&format!(" \u{b7} {programme}"));
                    }
                    let label = ui.label(RichText::new(line).weak());
                    if !scene.diagnostics.is_empty() {
                        label.on_hover_text(scene.diagnostics.join("\n"));
                        ui.label(
                            RichText::new(format!("{} note(s)", scene.diagnostics.len()))
                                .small()
                                .color(ui.visuals().warn_fg_color),
                        )
                        .on_hover_text(scene.diagnostics.join("\n"));
                    }
                    if edits.as_ref().is_some_and(|e| !e.fits(scene)) {
                        ui.label(
                            RichText::new("Edits are for a different version of this file")
                                .color(ui.visuals().warn_fg_color),
                        );
                    }
                }
            }
            ui.separator();
            ui.label("Glide");
            ui.add(
                egui::DragValue::new(&mut ramp)
                    .range(0.0..=10.0)
                    .speed(0.01)
                    .suffix(" s"),
            )
            .on_hover_text("How long a point placed by dragging in the room takes to move there");
            let edited = selected
                .as_ref()
                .zip(edits.as_ref())
                .is_some_and(|(key, edits)| edits.is_edited(key));
            if ui
                .add_enabled(edited, egui::Button::new("Reset element"))
                .on_hover_text("Back to the file's own movement for the selected element")
                .clicked()
            {
                acts.push(Act::Reset);
            }
            ui.separator();
            match export_progress {
                Some(progress) => {
                    ui.add(egui::ProgressBar::new(progress).desired_width(120.0).show_percentage());
                    if ui.button("Cancel").clicked() {
                        acts.push(Act::CancelExport);
                    }
                }
                None => {
                    let can_export = matches!(
                        &status,
                        SceneStatus::Ready(scene) if scene.format == crate::spatial::ObjectFormat::Adm
                    );
                    if ui
                        .add_enabled(can_export, egui::Button::new("Export ADM BWF\u{2026}"))
                        .on_hover_text(
                            "Write a new ADM BWF with these spatial edits. The audio is copied \
                             unchanged and this file is left as it is.",
                        )
                        .clicked()
                    {
                        acts.push(Act::Export);
                    }
                }
            }
        });
        if export_progress.is_some() {
            ctx.request_repaint_after(crate::app::ui_timing::PROGRESS_REFRESH);
        }
        if ramp != state.ramp_secs {
            self.tabs[tab_idx].spatial.ramp_secs = ramp;
        }
        let Some(scene) = scene else {
            if !matches!(status, SceneStatus::Failed(_)) {
                ctx.request_repaint_after(crate::app::ui_timing::PROGRESS_REFRESH);
            }
            self.apply_spatial_acts(tab_idx, &path, None, acts);
            return;
        };
        if is_playing {
            ctx.request_repaint();
        }
        ui.separator();

        let edits_ref = edits.as_deref().filter(|e| e.fits(&scene));
        let avail = ui.available_rect_before_wrap();
        let list_rect = Rect::from_min_size(
            avail.min,
            Vec2::new(LIST_W.min(avail.width() * 0.35), avail.height()),
        );
        let right = Rect::from_min_max(Pos2::new(list_rect.right() + 8.0, avail.top()), avail.max);
        let room_h = (right.height() - (RULER_H + 3.0 * LANE_H + 24.0)).clamp(140.0, ROOM_MAX);
        let room_rect = Rect::from_min_size(right.min, Vec2::new(right.width(), room_h));
        let lanes_rect =
            Rect::from_min_max(Pos2::new(right.left(), room_rect.bottom() + 8.0), right.max);
        ui.allocate_rect(avail, Sense::hover());

        // ---- Element list ----------------------------------------------
        let mut list_ui = ui.new_child(egui::UiBuilder::new().max_rect(list_rect));
        egui::ScrollArea::vertical()
            .id_salt(("spatial_elements", self.tabs[tab_idx].tab_id))
            .auto_shrink([false, false])
            .show(&mut list_ui, |ui| {
                for (title, objects) in [("Beds", false), ("Objects", true)] {
                    let items: Vec<&Element> = scene
                        .elements
                        .iter()
                        .filter(|e| e.is_object() == objects)
                        .collect();
                    if items.is_empty() {
                        continue;
                    }
                    ui.label(RichText::new(format!("{title} ({})", items.len())).strong());
                    for element in items {
                        ui.horizontal(|ui| {
                            // Mute and solo first: a long name would run under
                            // buttons laid out from the right.
                            if ui
                                .selectable_label(monitor.muted.contains(&element.key), "M")
                                .on_hover_text("Mute")
                                .clicked()
                            {
                                acts.push(Act::Mute(Arc::clone(&element.key)));
                            }
                            if ui
                                .selectable_label(monitor.soloed.contains(&element.key), "S")
                                .on_hover_text("Solo")
                                .clicked()
                            {
                                acts.push(Act::Solo(Arc::clone(&element.key)));
                            }
                            let (dot, _) =
                                ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                            ui.painter().circle_filled(
                                dot.center(),
                                4.0,
                                element_color(&element.key),
                            );
                            let is_selected = selected.as_deref() == Some(&*element.key);
                            let mut text = match &element.kind {
                                ElementKind::Bed { label, .. } => {
                                    format!("{label}  {}", element.name)
                                }
                                ElementKind::Object => element.name.to_string(),
                            };
                            if edits_ref.is_some_and(|e| e.is_edited(&element.key)) {
                                text.push_str(" \u{2022}");
                            }
                            let audible = monitor.audible(&element.key);
                            let mut rich = RichText::new(text);
                            if !audible {
                                rich = rich.weak();
                            }
                            let row =
                                ui.selectable_label(is_selected, rich)
                                    .on_hover_text(format!(
                                        "Track {} \u{b7} {}",
                                        element.track + 1,
                                        element.group
                                    ));
                            if row.clicked() {
                                acts.push(Act::Select(Arc::clone(&element.key)));
                            }
                        });
                    }
                    ui.add_space(6.0);
                }
            });

        let selected_element = selected.as_ref().and_then(|key| scene.element(key));
        let selected_keys = selected_element.map(|e| Arc::clone(keys_of(e, edits_ref)));
        let editable = selected_element.is_some_and(|e| e.is_object());

        // ---- The room --------------------------------------------------
        let side = room_rect
            .height()
            .min(room_rect.width() * 0.5 - 6.0)
            .max(80.0);
        let top = TopView {
            rect: Rect::from_min_size(room_rect.min, Vec2::splat(side)),
        };
        let side_view = SideView {
            rect: Rect::from_min_size(
                Pos2::new(top.rect.right() + 10.0, room_rect.top()),
                Vec2::new(
                    (room_rect.width() - side - 10.0).min(side * 1.2).max(80.0),
                    side,
                ),
            ),
        };
        let painter = ui.painter_at(room_rect);
        let visuals = ui.visuals().clone();
        let grid = Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.5));
        for view_rect in [top.rect, side_view.rect] {
            painter.rect_filled(view_rect, 4.0, visuals.extreme_bg_color);
        }
        // Walls, the listener, the speakers.
        painter.rect_stroke(top.rect.shrink(14.0), 0.0, grid, egui::StrokeKind::Inside);
        painter.circle_stroke(
            top.rect.center(),
            6.0,
            Stroke::new(1.5, visuals.text_color()),
        );
        let side_inner = side_view.rect.shrink(14.0);
        painter.rect_stroke(side_inner, 0.0, grid, egui::StrokeKind::Inside);
        let ear_y = side_view.to_screen([0.0, 0.0, 0.0]).y;
        painter.line_segment(
            [
                Pos2::new(side_inner.left(), ear_y),
                Pos2::new(side_inner.right(), ear_y),
            ],
            grid,
        );
        let font = FontId::proportional(10.0);
        painter.text(
            top.rect.center_top() + Vec2::new(0.0, 2.0),
            Align2::CENTER_TOP,
            "front",
            font.clone(),
            visuals.weak_text_color(),
        );
        painter.text(
            side_view.rect.right_top() + Vec2::new(-4.0, 2.0),
            Align2::RIGHT_TOP,
            "front \u{2192}",
            font.clone(),
            visuals.weak_text_color(),
        );
        for speaker in bed_layout() {
            let Some(cart) = speaker_cart(*speaker) else {
                continue;
            };
            let p = top.to_screen(cart);
            let size = Vec2::splat(7.0);
            if cart[2] > 0.5 {
                painter.rect_stroke(
                    Rect::from_center_size(p, size),
                    1.0,
                    Stroke::new(1.0, visuals.weak_text_color()),
                    egui::StrokeKind::Inside,
                );
            } else {
                painter.rect_filled(
                    Rect::from_center_size(p, size),
                    1.0,
                    visuals.weak_text_color().gamma_multiply(0.6),
                );
            }
            let q = side_view.to_screen(cart);
            painter.rect_filled(
                Rect::from_center_size(q, Vec2::splat(5.0)),
                1.0,
                visuals.weak_text_color().gamma_multiply(0.5),
            );
        }
        // The selected element's path around the playhead.
        if let (Some(element), Some(keys)) = (selected_element, selected_keys.as_ref()) {
            let color = element_color(&element.key);
            let points: Vec<[f32; 3]> = (0..=TRAIL_POINTS)
                .filter_map(|i| {
                    let t =
                        playhead - TRAIL_SECS + 2.0 * TRAIL_SECS * i as f64 / TRAIL_POINTS as f64;
                    cart_of(element, keys, t)
                })
                .collect();
            let top_line: Vec<Pos2> = points.iter().map(|c| top.to_screen(*c)).collect();
            let side_line: Vec<Pos2> = points.iter().map(|c| side_view.to_screen(*c)).collect();
            painter.add(egui::Shape::line(
                top_line,
                Stroke::new(1.5, color.gamma_multiply(0.5)),
            ));
            painter.add(egui::Shape::line(
                side_line,
                Stroke::new(1.5, color.gamma_multiply(0.5)),
            ));
        }
        // Every element where it is now.
        let mut hits: Vec<(Arc<str>, Pos2, Pos2)> = Vec::new();
        for element in &scene.elements {
            if element.is_lfe() || !element.is_active_at(playhead) {
                continue;
            }
            let keys = keys_of(element, edits_ref);
            let Some(cart) = cart_of(element, keys, playhead) else {
                continue;
            };
            let is_selected = selected.as_deref() == Some(&*element.key);
            let mut color = element_color(&element.key);
            if !monitor.audible(&element.key) {
                color = color.gamma_multiply(0.3);
            }
            let (p, q) = (top.to_screen(cart), side_view.to_screen(cart));
            let r = if is_selected { 7.0 } else { 4.5 };
            if element.is_object() {
                painter.circle_filled(p, r, color);
                painter.circle_filled(q, r, color);
            } else {
                painter.rect_filled(Rect::from_center_size(p, Vec2::splat(r * 1.6)), 1.5, color);
                painter.rect_filled(Rect::from_center_size(q, Vec2::splat(r * 1.6)), 1.5, color);
            }
            if is_selected {
                let ring = Stroke::new(2.0, visuals.strong_text_color());
                painter.circle_stroke(p, r + 2.5, ring);
                painter.circle_stroke(q, r + 2.5, ring);
            }
            hits.push((Arc::clone(&element.key), p, q));
        }
        // Picking and dragging in the room.
        for (view_index, view_rect) in [top.rect, side_view.rect].into_iter().enumerate() {
            let response = ui.interact(
                view_rect,
                ui.id().with(("spatial_room", view_index)),
                Sense::click_and_drag(),
            );
            let pointer = response.interact_pointer_pos();
            if response.clicked() || response.drag_started() {
                if let Some(at) = pointer {
                    let nearest = hits
                        .iter()
                        .map(|(key, p, q)| (key, if view_index == 0 { *p } else { *q }))
                        .filter(|(_, p)| p.distance(at) <= PICK_PX + 4.0)
                        .min_by(|a, b| a.1.distance(at).total_cmp(&b.1.distance(at)));
                    if let Some((key, _)) = nearest {
                        if selected.as_deref() != Some(&**key) {
                            acts.push(Act::Select(Arc::clone(key)));
                        }
                    }
                }
            }
            if !editable {
                continue;
            }
            if response.drag_started() {
                acts.push(Act::Begin(SpatialDrag::Room));
            }
            if response.dragged() {
                if let (Some(at), Some(element), Some(keys)) =
                    (pointer, selected_element, selected_keys.as_ref())
                {
                    let now = cart_of(element, keys, playhead).unwrap_or([0.0; 3]);
                    let cart = if view_index == 0 {
                        let (x, y) = top.from_screen(at);
                        [x, y, now[2]]
                    } else {
                        let (y, z) = side_view.from_screen(at);
                        [now[0], y, z]
                    };
                    acts.push(Act::Place(cart));
                }
            }
            if response.drag_stopped() {
                acts.push(Act::End);
            }
        }

        // ---- Lanes -----------------------------------------------------
        let duration = scene.shape.duration_secs().max(MIN_VIEW_SECS);
        let (mut view_start, mut view_secs) = if state.view_secs > 0.0 {
            (state.view_start, state.view_secs.min(duration))
        } else {
            (0.0, duration)
        };
        let shown = (view_start, view_secs);
        let label_w = 64.0;
        let axis = TimeAxis {
            left: lanes_rect.left() + label_w,
            width: (lanes_rect.width() - label_w).max(40.0),
            start: view_start,
            secs: view_secs,
        };
        let ruler = Rect::from_min_size(
            Pos2::new(axis.left, lanes_rect.top()),
            Vec2::new(axis.width, RULER_H),
        );
        let lane_painter = ui.painter_at(lanes_rect);
        let step = ruler_step(view_secs / axis.width as f64);
        let mut t = (view_start / step).ceil() * step;
        while t <= view_start + view_secs {
            let x = axis.x(t);
            lane_painter.line_segment(
                [
                    Pos2::new(x, ruler.bottom() - 4.0),
                    Pos2::new(x, ruler.bottom()),
                ],
                grid,
            );
            lane_painter.text(
                Pos2::new(x + 2.0, ruler.top()),
                Align2::LEFT_TOP,
                format_secs(t),
                font.clone(),
                visuals.weak_text_color(),
            );
            t += step;
        }
        let ruler_response = ui.interact(
            ruler,
            ui.id().with("spatial_ruler"),
            Sense::click_and_drag(),
        );
        if let Some(at) = ruler_response.interact_pointer_pos() {
            if ruler_response.clicked() || ruler_response.dragged() {
                acts.push(Act::Seek(axis.secs(at.x).clamp(0.0, duration)));
            }
        }
        let lanes_area = Rect::from_min_max(
            Pos2::new(lanes_rect.left(), ruler.bottom() + 2.0),
            lanes_rect.max,
        );
        // Wheel: scroll in time; with Ctrl, zoom around the pointer.
        if ui.rect_contains_pointer(lanes_area.union(ruler)) {
            let (scroll, zoom, pointer) = ui.input(|i| {
                (
                    i.smooth_scroll_delta,
                    i.modifiers.ctrl,
                    i.pointer.hover_pos(),
                )
            });
            if zoom && scroll.y != 0.0 {
                let factor = if scroll.y > 0.0 {
                    1.0 / ZOOM_STEP
                } else {
                    ZOOM_STEP
                };
                let anchor = pointer
                    .map(|p| axis.secs(p.x))
                    .unwrap_or(view_start + view_secs * 0.5);
                let new_secs = (view_secs * factor).clamp(MIN_VIEW_SECS, duration);
                view_start = anchor - (anchor - view_start) * new_secs / view_secs;
                view_secs = new_secs;
            } else if scroll.x != 0.0 || scroll.y != 0.0 {
                let delta = if scroll.x != 0.0 { scroll.x } else { scroll.y };
                view_start -= delta as f64 / axis.width as f64 * view_secs;
            }
            view_start = view_start.clamp(0.0, (duration - view_secs).max(0.0));
            if (view_start, view_secs) != shown {
                acts.push(Act::View {
                    start: view_start,
                    secs: view_secs,
                });
            }
        }
        if let (Some(element), Some(keys)) = (selected_element, selected_keys.as_ref()) {
            let labels = axis_labels(element.coords);
            let color = element_color(&element.key);
            for lane in 0..3 {
                let rect = Rect::from_min_size(
                    Pos2::new(lanes_area.left(), lanes_area.top() + lane as f32 * LANE_H),
                    Vec2::new(lanes_area.width(), LANE_H - 4.0),
                );
                if rect.bottom() > lanes_area.bottom() + 1.0 {
                    break;
                }
                let plot = Rect::from_min_max(Pos2::new(axis.left, rect.top()), rect.max);
                lane_painter.rect_filled(plot, 2.0, visuals.extreme_bg_color);
                lane_painter.text(
                    rect.left_center(),
                    Align2::LEFT_CENTER,
                    labels[lane],
                    font.clone(),
                    visuals.text_color(),
                );
                let (lo, hi) = axis_range(element.coords, lane);
                let to_y =
                    |v: f32| plot.bottom() - 3.0 - (v - lo) / (hi - lo) * (plot.height() - 6.0);
                let from_y = |y: f32| {
                    (lo + (plot.bottom() - 3.0 - y) / (plot.height() - 6.0) * (hi - lo))
                        .clamp(lo, hi)
                };
                let zero_y = to_y(0.0f32.clamp(lo, hi));
                lane_painter.line_segment(
                    [
                        Pos2::new(plot.left(), zero_y),
                        Pos2::new(plot.right(), zero_y),
                    ],
                    grid,
                );
                // The curve, one sample per pixel column.
                let columns = plot.width().max(1.0) as usize;
                let curve: Vec<Pos2> = (0..=columns)
                    .filter_map(|i| {
                        let x = plot.left() + i as f32;
                        let (pos, _) = sample_keyframes(keys, element.coords, axis.secs(x))?;
                        Some(Pos2::new(x, to_y(pos[lane])))
                    })
                    .collect();
                lane_painter.add(egui::Shape::line(curve, Stroke::new(1.5, color)));
                // Its keyframes.
                let mut key_points: Vec<(usize, Pos2)> = Vec::new();
                for (index, key) in keys.iter().enumerate() {
                    let x = axis.x(key.secs);
                    if x < plot.left() - POINT_R || x > plot.right() + POINT_R {
                        continue;
                    }
                    let p = Pos2::new(x, to_y(key.pos[lane]));
                    let picked = state.selected_key == Some(index);
                    lane_painter.circle_filled(
                        p,
                        if picked { POINT_R + 1.5 } else { POINT_R },
                        if picked {
                            visuals.strong_text_color()
                        } else {
                            color
                        },
                    );
                    if key.ramp_secs > 0.0 {
                        let start_x = axis.x(key.secs - key.ramp_secs).max(plot.left());
                        lane_painter.line_segment(
                            [
                                Pos2::new(start_x, plot.bottom() - 2.0),
                                Pos2::new(x, plot.bottom() - 2.0),
                            ],
                            Stroke::new(2.0, color.gamma_multiply(0.4)),
                        );
                    }
                    key_points.push((index, p));
                }
                let response = ui.interact(
                    plot,
                    ui.id().with(("spatial_lane", lane)),
                    Sense::click_and_drag(),
                );
                let pointer = response.interact_pointer_pos();
                let near = |at: Pos2| {
                    key_points
                        .iter()
                        .filter(|(_, p)| p.distance(at) <= PICK_PX)
                        .min_by(|a, b| a.1.distance(at).total_cmp(&b.1.distance(at)))
                        .map(|(index, _)| *index)
                };
                if response.double_clicked() && editable {
                    if let Some(at) = pointer {
                        match near(at) {
                            Some(index) => acts.push(Act::Remove(index)),
                            None => {
                                let secs = axis.secs(at.x).clamp(0.0, duration);
                                let mut pos = sample_keyframes(keys, element.coords, secs)
                                    .map(|(p, _)| p)
                                    .unwrap_or([0.0; 3]);
                                pos[lane] = from_y(at.y);
                                acts.push(Act::Insert { secs, pos });
                            }
                        }
                    }
                } else if response.clicked() {
                    if let Some(at) = pointer {
                        match near(at) {
                            Some(index) => acts.push(Act::SelectKey(Some(index))),
                            None => {
                                acts.push(Act::SelectKey(None));
                                acts.push(Act::Seek(axis.secs(at.x).clamp(0.0, duration)));
                            }
                        }
                    }
                }
                if response.drag_started() && editable {
                    if let Some(index) = pointer.and_then(near) {
                        acts.push(Act::SelectKey(Some(index)));
                        acts.push(Act::Begin(SpatialDrag::Lane { axis: lane, index }));
                    }
                }
                if response.dragged() {
                    if let (
                        Some(at),
                        Some(SpatialDrag::Lane {
                            axis: drag_axis,
                            index,
                        }),
                    ) = (pointer, state.drag.as_ref())
                    {
                        if *drag_axis == lane {
                            if let Some(key) = keys.get(*index) {
                                let mut pos = key.pos;
                                pos[lane] = from_y(at.y);
                                let secs = if ui.input(|i| i.modifiers.shift) {
                                    key.secs
                                } else {
                                    axis.secs(at.x).clamp(0.0, duration)
                                };
                                acts.push(Act::Move {
                                    index: *index,
                                    secs,
                                    pos,
                                });
                            }
                        }
                    }
                }
                if response.drag_stopped() {
                    acts.push(Act::End);
                }
                if response.hovered() && editable {
                    response.on_hover_text(
                        "Drag a point to move it (Shift: value only). Double-click: add a point, or remove one. Click: seek.",
                    );
                }
            }
            if !editable {
                lane_painter.text(
                    Pos2::new(axis.left + 6.0, lanes_area.top() + 3.0 * LANE_H - 6.0),
                    Align2::LEFT_BOTTOM,
                    "Bed channels stay at their speaker",
                    font.clone(),
                    visuals.weak_text_color(),
                );
            }
        }
        // The playhead, over the ruler and every lane.
        let px = axis.x(playhead);
        if px >= axis.left && px <= axis.left + axis.width {
            lane_painter.line_segment(
                [
                    Pos2::new(px, lanes_rect.top()),
                    Pos2::new(px, lanes_area.top() + 3.0 * LANE_H),
                ],
                Stroke::new(1.5, visuals.selection.stroke.color),
            );
        }
        // Delete removes the picked keyframe.
        if editable && state.selected_key.is_some() && !ctx.egui_wants_keyboard_input() {
            if ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
            {
                if let Some(index) = state.selected_key {
                    acts.push(Act::Remove(index));
                }
            }
        }
        if selected.as_ref() != state.selected.as_ref() {
            if let Some(key) = selected.clone() {
                acts.insert(0, Act::Select(key));
            }
        }
        self.apply_spatial_acts(tab_idx, &path, Some(&scene), acts);
    }

    fn apply_spatial_acts(
        &mut self,
        tab_idx: usize,
        path: &std::path::Path,
        scene: Option<&Arc<ObjectScene>>,
        acts: Vec<Act>,
    ) {
        for act in acts {
            match act {
                Act::Leave(view) => {
                    self.tabs[tab_idx].primary_view = view;
                    return;
                }
                Act::Select(key) => {
                    let state = &mut self.tabs[tab_idx].spatial;
                    if state.selected.as_ref() != Some(&key) {
                        state.selected = Some(key);
                        state.selected_key = None;
                    }
                }
                Act::Mute(key) => self.spatial_toggle_monitor(path, &key, false),
                Act::Solo(key) => self.spatial_toggle_monitor(path, &key, true),
                Act::Seek(secs) => self.playback_seek_to_source_time(self.mode, secs),
                Act::Begin(drag) => {
                    self.spatial_checkpoint(tab_idx);
                    self.tabs[tab_idx].spatial.drag = Some(drag);
                }
                Act::End => self.tabs[tab_idx].spatial.drag = None,
                Act::SelectKey(index) => self.tabs[tab_idx].spatial.selected_key = index,
                Act::Export => self.spatial_start_export(tab_idx),
                Act::CancelExport => self.spatial_cancel_export(),
                Act::View { start, secs } => {
                    let state = &mut self.tabs[tab_idx].spatial;
                    state.view_start = start;
                    state.view_secs = secs;
                }
                Act::Reset => {
                    let Some(key) = self.tabs[tab_idx].spatial.selected.clone() else {
                        continue;
                    };
                    self.spatial_checkpoint(tab_idx);
                    self.spatial_reset_element(path, &key);
                    self.tabs[tab_idx].spatial.selected_key = None;
                }
                Act::Place(_) | Act::Move { .. } | Act::Insert { .. } | Act::Remove(_) => {
                    let Some(scene) = scene else {
                        continue;
                    };
                    let Some(key) = self.tabs[tab_idx].spatial.selected.clone() else {
                        continue;
                    };
                    let Some(element) = scene.element(&key).filter(|e| e.is_object()) else {
                        continue;
                    };
                    let edits = self.spatial_edits_for(path);
                    let keys: Arc<[Keyframe]> = Arc::clone(
                        edits
                            .as_deref()
                            .filter(|e| e.fits(scene))
                            .map(|e| e.keyframes_for(element))
                            .unwrap_or(&element.keyframes),
                    );
                    let (next, picked) = match act {
                        Act::Place(cart) => {
                            let playhead = self.playback_current_source_time_sec().unwrap_or(0.0);
                            let ramp = self.tabs[tab_idx].spatial.ramp_secs as f64;
                            let (next, index) = put_keyframe(
                                &keys,
                                element.coords,
                                playhead,
                                from_cart(element.coords, cart),
                                ramp,
                            );
                            (next, Some(index))
                        }
                        Act::Move { index, secs, pos } => {
                            (move_keyframe(&keys, index, secs, pos), Some(index))
                        }
                        Act::Insert { secs, pos } => {
                            self.spatial_checkpoint(tab_idx);
                            let (next, index) =
                                put_keyframe(&keys, element.coords, secs, pos, f64::INFINITY);
                            (next, Some(index))
                        }
                        Act::Remove(index) => {
                            self.spatial_checkpoint(tab_idx);
                            (remove_keyframe(&keys, index), None)
                        }
                        _ => unreachable!(),
                    };
                    self.spatial_set_keyframes(path, scene, &key, next);
                    self.tabs[tab_idx].spatial.selected_key = picked;
                }
            }
        }
    }
}
