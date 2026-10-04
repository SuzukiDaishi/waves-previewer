//! Headphone monitoring's controls: the top bar's HRTF toggle, the row in the
//! settings window, and the "Virtual speakers (HRTF)" window. The rules live
//! in `app::hrtf_ops`; see `docs/MULTICHANNEL_SPEC.md`, section 6.

use std::path::PathBuf;

use egui::{Align2, Color32, FontId, Pos2, RichText, Sense, Stroke, Vec2};

use crate::app::hrtf_ops::{
    HrtfProfile, HrtfSettings, HrtfStatus, LfeMode, RoomPreset, SpeakerItem, SpeakerPlacement,
    LFE_GAIN_RANGE_DB, SPEAKER_GAIN_RANGE_DB, TRIM_RANGE_DB,
};
use crate::app::WavesPreviewer;
use crate::audio_channels::{Layout, SpeakerPos};

/// What the stereo switch does, wherever it is.
const STEREO_HOVER: &str = "On: stereo is heard from the room's L and R virtual speakers \
     (\u{b1}30\u{b0} in the standard room), as from a pair of monitors. Off: stereo goes \
     straight to the ears, and only 3 or more channels use the HRTF.";

/// The top bar toggle's width: "HRTF" at body size, and its frame.
pub(in crate::app) const HRTF_CHIP_W: f32 = 46.0;
/// How near a press must land to pick up a speaker in the views.
const PICK_RADIUS: f32 = 12.0;
const TOP_VIEW_SIZE: f32 = 260.0;
const SIDE_VIEW_W: f32 = 150.0;
/// Snap for dragged angles: whole degrees, or this with Shift held.
const COARSE_SNAP_DEG: f32 = 15.0;

enum WindowAction {
    LoadSofa,
    RemoveFile(PathBuf),
    OpenChannelLayout(PathBuf, usize),
}

/// A speaker the window lists and draws.
struct Shown {
    item: SpeakerItem,
    label: String,
    /// The source channel it plays, when the source uses it.
    channel: Option<usize>,
    placement: SpeakerPlacement,
}

/// The speakers of `layout` (the LFE has its own row), then, with
/// `show_all`, every other position.
fn shown_speakers(
    settings: &HrtfSettings,
    layout: &[Option<SpeakerPos>],
    show_all: bool,
) -> Vec<Shown> {
    let channels = layout.len();
    let mut out = Vec::new();
    for (ch, pos) in layout.iter().enumerate() {
        match pos {
            Some(SpeakerPos::Lfe) => {}
            Some(pos) => out.push(Shown {
                item: SpeakerItem::Named(*pos),
                label: pos.label(layout).to_string(),
                channel: Some(ch),
                placement: settings.placement(*pos, layout),
            }),
            None => out.push(Shown {
                item: SpeakerItem::Unlabeled { channels, ch },
                label: format!("Ch {}", ch + 1),
                channel: Some(ch),
                placement: settings.unlabeled_placement(channels, ch),
            }),
        }
    }
    if show_all {
        for pos in SpeakerPos::ALL {
            if pos.is_lfe() || layout.contains(&Some(pos)) {
                continue;
            }
            out.push(Shown {
                item: SpeakerItem::Named(pos),
                label: pos.label(layout).to_string(),
                channel: None,
                placement: settings.placement(pos, layout),
            });
        }
    }
    out
}

fn snap(deg: f32, coarse: bool) -> f32 {
    let step = if coarse { COARSE_SNAP_DEG } else { 1.0 };
    (deg / step).round() * step
}

fn speaker_color(used: bool, selected: bool, visuals: &egui::Visuals) -> Color32 {
    if selected {
        visuals.selection.bg_fill
    } else if used {
        visuals.strong_text_color()
    } else {
        visuals.weak_text_color()
    }
}

/// The nearest of `points` to `at`, within [`PICK_RADIUS`].
fn pick(points: &[(SpeakerItem, Pos2)], at: Pos2) -> Option<SpeakerItem> {
    points
        .iter()
        .map(|(item, p)| (*item, p.distance(at)))
        .filter(|(_, d)| *d <= PICK_RADIUS)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(item, _)| item)
}

/// The room from above: front up, the ear-level circle outside, the upper
/// layer inside it (a speaker's distance from the centre is the cosine of
/// its elevation), a lower layer drawn hollow. Dragging a speaker turns it.
fn top_view(
    ui: &mut egui::Ui,
    settings: &mut HrtfSettings,
    layout: &[Option<SpeakerPos>],
    shown: &[Shown],
    selected: &mut Option<SpeakerItem>,
    dragging: &mut Option<SpeakerItem>,
) {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::splat(TOP_VIEW_SIZE), Sense::click_and_drag());
    let response = response.on_hover_text(
        "From above, front up. Drag a speaker to turn it (Shift: 15\u{b0} steps). \
         The inner circle is 45\u{b0} up; hollow dots are below ear level.",
    );
    let visuals = ui.visuals().clone();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);
    let center = rect.center();
    let radius = rect.width() * 0.5 - 22.0;
    let ring = Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.6));
    painter.circle_stroke(center, radius, ring);
    painter.circle_stroke(center, radius * std::f32::consts::FRAC_1_SQRT_2, ring);
    // The listener: a head with a nose pointing front.
    painter.circle_stroke(center, 9.0, Stroke::new(1.5, visuals.text_color()));
    painter.line_segment(
        [
            center + Vec2::new(-4.0, -8.0),
            center + Vec2::new(0.0, -14.0),
        ],
        Stroke::new(1.5, visuals.text_color()),
    );
    painter.line_segment(
        [
            center + Vec2::new(4.0, -8.0),
            center + Vec2::new(0.0, -14.0),
        ],
        Stroke::new(1.5, visuals.text_color()),
    );
    let font = FontId::proportional(11.0);
    let at = |p: SpeakerPlacement| -> Pos2 {
        let az = p.azimuth_deg.to_radians();
        let r = radius * p.elevation_deg.to_radians().cos();
        center + Vec2::new(az.sin() * r, -az.cos() * r)
    };
    let mut points = Vec::with_capacity(shown.len());
    // Unused positions first, so the source's own speakers draw on top.
    let mut order: Vec<&Shown> = shown.iter().collect();
    order.sort_by_key(|s| s.channel.is_some());
    for s in order {
        let p = at(s.placement);
        let is_selected = *selected == Some(s.item);
        let color = speaker_color(s.channel.is_some(), is_selected, &visuals);
        let size = if is_selected { 7.0 } else { 5.5 };
        if s.placement.elevation_deg < -0.5 {
            painter.circle_stroke(p, size, Stroke::new(2.0, color));
        } else {
            painter.circle_filled(p, size, color);
        }
        let outward = (p - center).normalized();
        let outward = if outward.is_finite() {
            outward
        } else {
            Vec2::new(0.0, -1.0)
        };
        painter.text(
            p + outward * 13.0,
            Align2::CENTER_CENTER,
            &s.label,
            font.clone(),
            color,
        );
        points.push((s.item, p));
    }
    painter.text(
        rect.left_top() + Vec2::new(6.0, 4.0),
        Align2::LEFT_TOP,
        "TOP",
        font.clone(),
        visuals.weak_text_color(),
    );
    if response.clicked() || response.drag_started() {
        if let Some(item) = response
            .interact_pointer_pos()
            .and_then(|p| pick(&points, p))
        {
            *selected = Some(item);
            if response.drag_started() {
                *dragging = Some(item);
            }
        }
    }
    if response.dragged() {
        if let (Some(item), Some(p)) = (*dragging, response.interact_pointer_pos()) {
            let d = p - center;
            if d.length() > 4.0 {
                let coarse = ui.input(|i| i.modifiers.shift);
                let mut placement = settings.item_placement(item, layout);
                placement.azimuth_deg = snap(d.x.atan2(-d.y).to_degrees(), coarse);
                settings.set_item_placement(item, placement);
            }
        }
    }
    if response.drag_stopped() {
        *dragging = None;
    }
}

/// The picked speaker from the side: front to the right, up at the top.
/// Dragging it raises or lowers it.
fn side_view(
    ui: &mut egui::Ui,
    settings: &mut HrtfSettings,
    layout: &[Option<SpeakerPos>],
    shown: &[Shown],
    selected: &mut Option<SpeakerItem>,
    dragging: &mut Option<SpeakerItem>,
) {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(SIDE_VIEW_W, TOP_VIEW_SIZE),
        Sense::click_and_drag(),
    );
    let response = response.on_hover_text(
        "The picked speaker from the side. Drag it up or down to change its elevation \
         (Shift: 15\u{b0} steps); below the line is a lower layer.",
    );
    let visuals = ui.visuals().clone();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);
    let center = Pos2::new(rect.left() + 26.0, rect.center().y);
    let radius = (rect.width() - 46.0).min(rect.height() * 0.5 - 22.0);
    let ring = Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.6));
    let arc: Vec<Pos2> = (0..=36)
        .map(|i| {
            let el = (-90.0 + i as f32 * 5.0).to_radians();
            center + Vec2::new(el.cos() * radius, -el.sin() * radius)
        })
        .collect();
    painter.add(egui::Shape::line(arc, ring));
    painter.line_segment([center, center + Vec2::new(radius, 0.0)], ring);
    painter.circle_stroke(center, 7.0, Stroke::new(1.5, visuals.text_color()));
    let font = FontId::proportional(11.0);
    for (deg, text) in [
        (90.0f32, "90\u{b0}"),
        (45.0, "45\u{b0}"),
        (0.0, "0\u{b0}"),
        (-45.0, "-45\u{b0}"),
    ] {
        let el = deg.to_radians();
        let p = center + Vec2::new(el.cos() * (radius + 12.0), -el.sin() * (radius + 12.0));
        painter.text(
            p,
            Align2::CENTER_CENTER,
            text,
            font.clone(),
            visuals.weak_text_color(),
        );
    }
    let picked = selected.and_then(|item| shown.iter().find(|s| s.item == item));
    let mut points = Vec::new();
    if let Some(s) = picked {
        let el = s.placement.elevation_deg.to_radians();
        let p = center + Vec2::new(el.cos() * radius, -el.sin() * radius);
        let color = visuals.selection.bg_fill;
        painter.line_segment([center, p], Stroke::new(1.0, color));
        painter.circle_filled(p, 7.0, color);
        painter.text(
            rect.left_bottom() + Vec2::new(6.0, -4.0),
            Align2::LEFT_BOTTOM,
            format!("{}  {:.0}\u{b0}", s.label, s.placement.elevation_deg),
            font.clone(),
            visuals.text_color(),
        );
        points.push((s.item, p));
    } else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Pick a speaker",
            font.clone(),
            visuals.weak_text_color(),
        );
    }
    // Top right: the 90 degree mark is at the top left.
    painter.text(
        rect.right_top() + Vec2::new(-6.0, 4.0),
        Align2::RIGHT_TOP,
        "SIDE",
        font,
        visuals.weak_text_color(),
    );
    if response.drag_started() {
        if let Some(item) = response
            .interact_pointer_pos()
            .and_then(|p| pick(&points, p))
        {
            *dragging = Some(item);
        }
    }
    if response.dragged() {
        if let (Some(item), Some(p)) = (*dragging, response.interact_pointer_pos()) {
            if picked.is_some_and(|s| s.item == item) {
                let d = p - center;
                let coarse = ui.input(|i| i.modifiers.shift);
                let mut placement = settings.item_placement(item, layout);
                placement.elevation_deg =
                    snap((-d.y).atan2(d.x.max(1.0)).to_degrees(), coarse).clamp(-90.0, 90.0);
                settings.set_item_placement(item, placement);
            }
        }
    }
    if response.drag_stopped() {
        *dragging = None;
    }
}

impl WavesPreviewer {
    /// The HRTFs on offer: the bundled one, then the user's files.
    fn hrtf_profiles(&self) -> Vec<HrtfProfile> {
        let mut profiles = vec![HrtfProfile::Builtin];
        profiles.extend(self.hrtf.user_files.iter().cloned().map(HrtfProfile::File));
        if !profiles.contains(&self.hrtf.profile) {
            profiles.push(self.hrtf.profile.clone());
        }
        profiles
    }

    /// One line on what the HRTF is doing, and whether it is a problem.
    fn hrtf_status_line(&self) -> (String, bool) {
        let profile = self.hrtf.profile.label();
        match &self.hrtf_runtime.status {
            HrtfStatus::Off => ("Headphones (HRTF): off".to_string(), false),
            HrtfStatus::Loading => (format!("Loading {profile}\u{2026}"), false),
            HrtfStatus::Failed(message) => {
                (format!("{profile} could not be loaded: {message}"), true)
            }
            HrtfStatus::Bypassed(why) => (format!("{profile}: on, not used now ({why})"), false),
            HrtfStatus::Active { channels } => {
                (format!("{profile}: {channels} channels to the ears"), false)
            }
        }
    }

    /// The top bar's toggle: click for on/off, right-click for the HRTF and
    /// the window.
    pub(in crate::app) fn ui_hrtf_chip(&mut self, ui: &mut egui::Ui) {
        let on = self.hrtf.enabled;
        let (line, problem) = self.hrtf_status_line();
        let working = matches!(self.hrtf_runtime.status, HrtfStatus::Active { .. });
        let mut text = RichText::new("HRTF");
        if problem {
            text = text.color(ui.visuals().warn_fg_color);
        } else if on && !working {
            text = text.color(ui.visuals().weak_text_color());
        }
        let response = ui
            .add_sized([HRTF_CHIP_W, 22.0], egui::Button::selectable(on, text))
            .on_hover_text(format!(
                "{line}\n\nHeadphone monitoring through an HRTF: stereo from the L and R \
                 speakers, surround from every speaker. Click: on / off. Right-click: \
                 HRTF and virtual speakers."
            ));
        if response.clicked() {
            self.set_hrtf_enabled(!on);
        }
        response.context_menu(|ui| self.ui_hrtf_menu(ui));
    }

    fn ui_hrtf_menu(&mut self, ui: &mut egui::Ui) {
        let mut enabled = self.hrtf.enabled;
        if ui.checkbox(&mut enabled, "Headphones (HRTF)").changed() {
            self.set_hrtf_enabled(enabled);
        }
        self.ui_hrtf_stereo_checkbox(ui);
        ui.separator();
        for profile in self.hrtf_profiles() {
            if ui
                .radio(self.hrtf.profile == profile, profile.label())
                .clicked()
            {
                self.set_hrtf_profile(profile);
                ui.close();
            }
        }
        if ui.button("Load SOFA\u{2026}").clicked() {
            ui.close();
            self.pick_and_add_hrtf_file();
        }
        ui.separator();
        if ui.button("Virtual speakers\u{2026}").clicked() {
            self.open_hrtf_window();
            ui.close();
        }
    }

    /// Stereo through the HRTF, from the room's L and R speakers, or straight
    /// to the ears.
    fn ui_hrtf_stereo_checkbox(&mut self, ui: &mut egui::Ui) {
        let mut stereo = self.hrtf.stereo_too;
        if ui
            .checkbox(&mut stereo, "Stereo from L / R speakers")
            .on_hover_text(STEREO_HOVER)
            .changed()
        {
            self.hrtf.stereo_too = stereo;
            self.hrtf_settings_changed(true);
        }
    }

    /// The settings window's row, under the output device.
    pub(in crate::app) fn ui_hrtf_settings_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let mut enabled = self.hrtf.enabled;
            if ui
                .checkbox(&mut enabled, "Headphones (HRTF)")
                .on_hover_text(
                    "Monitor on headphones: every channel is heard from where its virtual \
                     speaker stands -- stereo from L and R, surround from every speaker. \
                     Monitoring only; nothing is written.",
                )
                .changed()
            {
                self.set_hrtf_enabled(enabled);
            }
            self.ui_hrtf_stereo_checkbox(ui);
            let mut profile = self.hrtf.profile.clone();
            egui::ComboBox::from_id_salt("hrtf_settings_profile")
                .selected_text(profile.label())
                .width(180.0)
                .show_ui(ui, |ui| {
                    for option in self.hrtf_profiles() {
                        let label = option.label();
                        ui.selectable_value(&mut profile, option, label);
                    }
                });
            if profile != self.hrtf.profile {
                self.set_hrtf_profile(profile);
            }
            if ui.button("Virtual speakers\u{2026}").clicked() {
                self.open_hrtf_window();
            }
        });
        if self.hrtf.enabled {
            let (line, problem) = self.hrtf_status_line();
            let color = if problem {
                ui.visuals().warn_fg_color
            } else {
                ui.visuals().weak_text_color()
            };
            ui.label(RichText::new(line).small().color(color));
            if self.audio_channel_map_direct {
                ui.label(
                    RichText::new(
                        "The HRTF takes precedence over direct channel mapping for the sources it plays.",
                    )
                    .small()
                    .weak(),
                );
            }
        }
    }

    pub(in crate::app) fn ui_hrtf_window(&mut self, ctx: &egui::Context) {
        // Asked for from the mini meter's menu, which cannot reach `self`.
        if let Some(tab) = self.active_tab.and_then(|idx| self.tabs.get_mut(idx)) {
            if std::mem::take(&mut tab.mini_meter.hrtf_window_requested) {
                self.hrtf_runtime.window_open = true;
            }
        }
        // A change made mid-drag is saved once the pointer is let go.
        if self.hrtf_runtime.pending_save && !ctx.input(|i| i.pointer.any_down()) {
            self.hrtf_runtime.pending_save = false;
            self.save_prefs();
        }
        if !self.hrtf_runtime.window_open {
            return;
        }
        let source = self.hrtf_window_source();
        let layout: Layout = source
            .as_ref()
            .map(|(_, layout)| layout.clone())
            .unwrap_or_default();
        let source_path = source.as_ref().and_then(|(path, _)| path.clone());
        let mut settings = self.hrtf.clone();
        let mut show_all = self.hrtf_runtime.show_all_positions || layout.is_empty();
        let mut selected = self.hrtf_runtime.selected;
        let mut dragging = self.hrtf_runtime.dragging;
        let (status_line, problem) = self.hrtf_status_line();
        let loading = matches!(self.hrtf_runtime.status, HrtfStatus::Loading);
        let info = self.hrtf_loaded().map(|hrtf| hrtf.describe());
        let profiles = self.hrtf_profiles();
        let mut action: Option<WindowAction> = None;
        let mut open = true;
        let scroll_target = self.begin_floating_scroll_surface("hrtf_window");
        let scroll_guard = self.pointer_scroll_input_guard(scroll_target, ctx);
        // Below the menu and the top bar, not over them; and never taller
        // than the screen, or egui moves it to fit and it slides out from
        // under the pointer.
        let screen = ctx.content_rect();
        let body_max_h = (screen.height() - 160.0).max(240.0);
        let shown_window = egui::Window::new("Virtual speakers (HRTF)")
            .open(&mut open)
            .resizable(true)
            .default_width(600.0)
            .default_pos(screen.left_top() + Vec2::new(40.0, 100.0))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("hrtf_window_body")
                    .max_height(body_max_h)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        // ---- The HRTF ------------------------------------------------
                        ui.horizontal_wrapped(|ui| {
                            ui.toggle_value(&mut settings.enabled, "HRTF On")
                                .on_hover_text("Headphone monitoring through the HRTF below.");
                            ui.label("Profile");
                            egui::ComboBox::from_id_salt("hrtf_window_profile")
                                .selected_text(settings.profile.label())
                                .width(220.0)
                                .show_ui(ui, |ui| {
                                    for profile in &profiles {
                                        ui.selectable_value(
                                            &mut settings.profile,
                                            profile.clone(),
                                            profile.label(),
                                        );
                                    }
                                });
                            if ui
                                .button("Load SOFA\u{2026}")
                                .on_hover_text(
                                    "Add an HRTF in SOFA format (SimpleFreeFieldHRIR) and use it.",
                                )
                                .clicked()
                            {
                                action = Some(WindowAction::LoadSofa);
                            }
                            if let HrtfProfile::File(path) = &settings.profile {
                                if ui
                                    .button("Remove")
                                    .on_hover_text(
                                        "Take it off the list. The file stays where it is.",
                                    )
                                    .clicked()
                                {
                                    action = Some(WindowAction::RemoveFile(path.clone()));
                                }
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            if loading {
                                ui.spinner();
                            }
                            let color = if problem {
                                ui.visuals().warn_fg_color
                            } else {
                                ui.visuals().weak_text_color()
                            };
                            ui.label(RichText::new(&status_line).small().color(color));
                            if let Some(info) = &info {
                                ui.label(RichText::new(format!("\u{b7} {info}")).small().weak());
                            }
                        });
                        ui.separator();

                        // ---- The room -------------------------------------------------
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Room");
                            egui::ComboBox::from_id_salt("hrtf_window_room")
                                .selected_text(settings.room.label())
                                .width(170.0)
                                .show_ui(ui, |ui| {
                                    for room in RoomPreset::ALL {
                                        ui.selectable_value(&mut settings.room, room, room.label());
                                    }
                                });
                            if ui
                                .button("Reset placements")
                                .on_hover_text(
                                    "Forget every speaker you moved: back to the room's positions.",
                                )
                                .clicked()
                            {
                                settings.overrides = [None; SpeakerPos::ALL.len()];
                                settings.unlabeled.clear();
                            }
                            ui.label("Output trim");
                            ui.add(
                                egui::DragValue::new(&mut settings.trim_db)
                                    .range(TRIM_RANGE_DB)
                                    .speed(0.1)
                                    .suffix(" dB"),
                            );
                            ui.checkbox(&mut settings.stereo_too, "Stereo from L / R speakers")
                                .on_hover_text(STEREO_HOVER);
                        });
                        ui.add_space(4.0);

                        // ---- Where the speakers stand ----------------------------------
                        let shown = shown_speakers(&settings, &layout, show_all);
                        ui.horizontal(|ui| {
                            top_view(
                                ui,
                                &mut settings,
                                &layout,
                                &shown,
                                &mut selected,
                                &mut dragging,
                            );
                            side_view(
                                ui,
                                &mut settings,
                                &layout,
                                &shown,
                                &mut selected,
                                &mut dragging,
                            );
                        });
                        ui.checkbox(&mut show_all, "Show all positions")
                            .on_hover_text(
                                "List every speaker position, not only the ones this source uses.",
                            );
                        egui::ScrollArea::vertical()
                            .max_height(240.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                egui::Grid::new("hrtf_speakers")
                                    .num_columns(6)
                                    .striped(true)
                                    .show(ui, |ui| {
                                        for header in
                                            ["Speaker", "Ch", "Azimuth", "Elevation", "Gain", ""]
                                        {
                                            ui.label(RichText::new(header).small().weak());
                                        }
                                        ui.end_row();
                                        for s in &shown {
                                            let is_selected = selected == Some(s.item);
                                            if ui.selectable_label(is_selected, &s.label).clicked()
                                            {
                                                selected = Some(s.item);
                                            }
                                            ui.label(s.channel.map_or_else(
                                                || "\u{2014}".to_string(),
                                                |ch| format!("{}", ch + 1),
                                            ));
                                            let mut p = s.placement;
                                            let az = ui.add(
                                                egui::DragValue::new(&mut p.azimuth_deg)
                                                    .range(-180.0..=180.0)
                                                    .speed(0.5)
                                                    .suffix("\u{b0}"),
                                            );
                                            let el = ui.add(
                                                egui::DragValue::new(&mut p.elevation_deg)
                                                    .range(-90.0..=90.0)
                                                    .speed(0.5)
                                                    .suffix("\u{b0}"),
                                            );
                                            let gain = ui.add(
                                                egui::DragValue::new(&mut p.gain_db)
                                                    .range(SPEAKER_GAIN_RANGE_DB)
                                                    .speed(0.1)
                                                    .suffix(" dB"),
                                            );
                                            if az.changed() || el.changed() || gain.changed() {
                                                settings.set_item_placement(s.item, p);
                                                selected = Some(s.item);
                                            }
                                            if settings.item_edited(s.item) {
                                                if ui
                                                    .small_button("\u{21ba}")
                                                    .on_hover_text("Back to the room's position")
                                                    .clicked()
                                                {
                                                    settings.reset_item(s.item);
                                                }
                                            } else {
                                                ui.label("");
                                            }
                                            ui.end_row();
                                        }
                                    });
                            });

                        // ---- LFE ------------------------------------------------------
                        if show_all || layout.contains(&Some(SpeakerPos::Lfe)) {
                            ui.horizontal_wrapped(|ui| {
                                ui.label("LFE");
                                egui::ComboBox::from_id_salt("hrtf_window_lfe")
                                    .selected_text(match settings.lfe {
                                        LfeMode::BothEars => "Both ears",
                                        LfeMode::Off => "Off",
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut settings.lfe,
                                            LfeMode::BothEars,
                                            "Both ears",
                                        );
                                        ui.selectable_value(&mut settings.lfe, LfeMode::Off, "Off");
                                    })
                                    .response
                                    .on_hover_text(
                                        "The LFE has no direction: it goes to both ears alike, \
                                 without the HRTF.",
                                    );
                                let on = settings.lfe == LfeMode::BothEars;
                                ui.add_enabled(
                                    on,
                                    egui::DragValue::new(&mut settings.lfe_gain_db)
                                        .range(LFE_GAIN_RANGE_DB)
                                        .speed(0.1)
                                        .suffix(" dB"),
                                );
                                ui.add_enabled(
                                    on,
                                    egui::Checkbox::new(
                                        &mut settings.lfe_lowpass,
                                        "Low-pass 120 Hz",
                                    ),
                                );
                            });
                        }
                        ui.separator();

                        // ---- The source -----------------------------------------------
                        ui.horizontal_wrapped(|ui| {
                            let text = match (&source_path, layout.len()) {
                                (_, 0) => "Nothing open: placing the speakers only.".to_string(),
                                (Some(path), n) => format!(
                                    "{} \u{2014} {n} channels",
                                    path.file_name()
                                        .map(|n| n.to_string_lossy().into_owned())
                                        .unwrap_or_default()
                                ),
                                (None, n) => format!("Playing {n} channels"),
                            };
                            ui.label(RichText::new(text).weak());
                            if let Some(path) = &source_path {
                                if !layout.is_empty()
                                    && ui
                                        .button("Channel layout\u{2026}")
                                        .on_hover_text(
                                            "Which speaker each channel of this file is.",
                                        )
                                        .clicked()
                                {
                                    action = Some(WindowAction::OpenChannelLayout(
                                        path.clone(),
                                        layout.len(),
                                    ));
                                }
                            }
                        });
                    });
            });
        drop(scroll_guard);
        if let Some(shown_window) = shown_window.as_ref() {
            self.register_scroll_surface(scroll_target, &shown_window.response);
        }
        self.hrtf_runtime.window_open = open;
        self.hrtf_runtime.show_all_positions = show_all;
        self.hrtf_runtime.selected = selected;
        self.hrtf_runtime.dragging = dragging;
        if settings != self.hrtf {
            self.hrtf = settings;
            // A drag changes the placement every frame: hear it now, save it
            // when the pointer is let go.
            let mid_drag = ctx.input(|i| i.pointer.any_down());
            self.hrtf_settings_changed(!mid_drag);
            self.hrtf_runtime.pending_save |= mid_drag;
        }
        match action {
            Some(WindowAction::LoadSofa) => self.pick_and_add_hrtf_file(),
            Some(WindowAction::RemoveFile(path)) => self.remove_hrtf_file(&path),
            Some(WindowAction::OpenChannelLayout(path, channels)) => {
                self.open_channel_layout_editor(path, channels)
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_lists_the_sources_speakers_and_its_unlabeled_channels() {
        let settings = HrtfSettings::default();
        let layout = vec![
            Some(SpeakerPos::Fl),
            Some(SpeakerPos::Fr),
            Some(SpeakerPos::Lfe),
            None,
        ];
        let shown = shown_speakers(&settings, &layout, false);
        let labels: Vec<&str> = shown.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels.len(), 3, "the LFE has its own row: {labels:?}");
        assert_eq!(shown[2].item, SpeakerItem::Unlabeled { channels: 4, ch: 3 });
        let all = shown_speakers(&settings, &layout, true);
        assert_eq!(
            all.len(),
            SpeakerPos::ALL.len() - 1 + 1,
            "every position but the LFE, plus Ch 4"
        );
        assert!(all.iter().filter(|s| s.channel.is_none()).count() >= 19);
    }

    #[test]
    fn dragged_angles_snap() {
        assert_eq!(snap(31.4, false), 31.0);
        assert_eq!(snap(31.4, true), 30.0);
        assert_eq!(snap(-97.0, true), -90.0);
    }

    #[test]
    fn a_press_picks_the_nearest_speaker_within_reach() {
        let a = SpeakerItem::Named(SpeakerPos::Fl);
        let b = SpeakerItem::Named(SpeakerPos::Fr);
        let points = [(a, Pos2::new(0.0, 0.0)), (b, Pos2::new(10.0, 0.0))];
        assert_eq!(pick(&points, Pos2::new(7.0, 0.0)), Some(b));
        assert_eq!(pick(&points, Pos2::new(50.0, 50.0)), None);
    }
}
