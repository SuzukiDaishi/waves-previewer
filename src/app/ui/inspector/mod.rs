//! The editor's Inspector: choose a tool, see what it will act on, set it
//! up, hear it, apply it.
//!
//! Top to bottom: the palette (categories, then the tools of one), the
//! tool's header, the target bar, the tool's own settings (they scroll), and
//! a footer that never scrolls away -- what is being heard, Preview / A/B,
//! Discard and Apply. The panels under `tools/` draw settings only: every
//! tool is previewed, applied and undone through the same footer, by the
//! rules in `editor_tool.rs` and `tool_session.rs`.

mod plots;
mod tools;
mod views;
pub(in crate::app) mod widgets;

use std::time::{Duration, Instant};

use egui::{Color32, RichText};

use crate::app::editor_tool::{PreviewKind, TargetRule, ToolCategory};
use crate::app::inspector_ops::{InspectorAction, ToolCommand};
use crate::app::tool_render::RenderTarget;
use crate::app::tool_session::{DraftSnapshot, SessionPhase};
use crate::app::types::{ToolKind, ViewMode};
use crate::app::WavesPreviewer;
use widgets::Gestures;

/// The space the Inspector is given.
pub(in crate::app) struct InspectorFrame {
    pub width: f32,
    pub height: f32,
    pub scroll_source: egui::scroll_area::ScrollSource,
}

/// Height kept for the footer below the scrolling settings.
const FOOTER_HEIGHT: f32 = 64.0;
/// How long "Applied" stays in the footer after an apply lands.
const APPLIED_NOTICE: Duration = Duration::from_secs(6);

const DOT_OFF: Color32 = Color32::from_rgb(140, 146, 158);
const DOT_LIVE: Color32 = Color32::from_rgb(110, 210, 120);
const DOT_PENDING: Color32 = Color32::from_rgb(235, 185, 80);
const DOT_ERROR: Color32 = Color32::from_rgb(235, 110, 100);
const DOT_AB: Color32 = Color32::from_rgb(110, 170, 240);

impl WavesPreviewer {
    pub(in crate::app) fn ui_editor_inspector(
        &mut self,
        ui: &mut egui::Ui,
        tab_idx: usize,
        frame: &InspectorFrame,
    ) {
        let rect =
            egui::Rect::from_min_size(ui.cursor().min, egui::vec2(frame.width, frame.height));
        self.editor_inspector_rect = Some(rect);
        let mut actions: Vec<InspectorAction> = Vec::new();
        ui.allocate_ui_with_layout(
            egui::vec2(frame.width, frame.height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(frame.width);
                ui.heading("Inspector");
                ui.separator();
                let Some(tab) = self.tabs.get(tab_idx) else {
                    return;
                };
                if tab.read_only {
                    ui.label(
                        RichText::new("Read-only source")
                            .small()
                            .color(Color32::from_rgb(220, 180, 110)),
                    );
                    widgets::note(
                        ui,
                        "A video's audio can be played and measured, but not edited or written back out.",
                    );
                    ui.disable();
                }
                let view = tab.leaf_view_mode();
                match view {
                    ViewMode::Waveform | ViewMode::Spectrogram | ViewMode::Log => {
                        if matches!(view, ViewMode::Spectrogram | ViewMode::Log) {
                            self.ui_inspector_display_row(ui, tab_idx);
                        }
                        self.ui_inspector_palette(ui, tab_idx, view, &mut actions);
                        let tool = self.tabs[tab_idx].active_tool;
                        if !tool.offered_in(view) {
                            ui.add_space(6.0);
                            widgets::note(ui, "Pick a tool for this view.");
                            return;
                        }
                        ui.separator();
                        self.ui_inspector_tool_header(ui, tab_idx, &mut actions);
                        self.ui_inspector_target(ui, tab_idx, &mut actions);
                        let body_h = (ui.available_height() - FOOTER_HEIGHT).max(80.0);
                        egui::ScrollArea::vertical()
                            .scroll_source(frame.scroll_source)
                            .id_salt(("editor_inspector_scroll", tab_idx))
                            .max_height(body_h)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                                self.ui_inspector_tool_panel(ui, tab_idx, &mut actions);
                            });
                        ui.separator();
                        self.ui_inspector_footer(ui, tab_idx, &mut actions);
                    }
                    ViewMode::Mel => {
                        widgets::note(ui, "The Mel view is for looking only; switch to Spec or Wave to edit.");
                    }
                    _ => {
                        egui::ScrollArea::vertical()
                            .scroll_source(frame.scroll_source)
                            .id_salt(("editor_inspector_scroll", tab_idx))
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                self.ui_inspector_analysis_view(ui, tab_idx, view, &mut actions);
                            });
                    }
                }
            },
        );
        self.apply_inspector_actions(tab_idx, actions);
    }

    /// The spectrogram views' own setting: whether the waveform is drawn
    /// over them (and with it the preview's green waveform).
    fn ui_inspector_display_row(&mut self, ui: &mut egui::Ui, tab_idx: usize) {
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        ui.checkbox(&mut tab.show_waveform_overlay, "Waveform overlay")
            .on_hover_text("Draw the waveform (and a tool's green preview) over the spectrogram");
    }

    fn ui_inspector_palette(
        &mut self,
        ui: &mut egui::Ui,
        tab_idx: usize,
        view: ViewMode,
        actions: &mut Vec<InspectorAction>,
    ) {
        let categories = ToolCategory::for_view(view);
        let active = self.tabs[tab_idx].active_tool;
        let active_category = active.spec().category;
        let shown_category = if categories.contains(&active_category) {
            active_category
        } else {
            categories[0]
        };
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
            for &category in categories {
                let selected = category == shown_category;
                let response = ui
                    .selectable_label(selected, RichText::new(category.label()).small())
                    .on_hover_text(
                        category
                            .tools()
                            .iter()
                            .map(|t| t.spec().label)
                            .collect::<Vec<_>>()
                            .join(" · "),
                    );
                if response.clicked() && !selected {
                    let tool = self
                        .inspector_last_tool
                        .get(&category)
                        .copied()
                        .unwrap_or(category.tools()[0]);
                    actions.push(InspectorAction::SelectTool(tool));
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
            for &tool in shown_category.tools() {
                let spec = tool.spec();
                let response = ui
                    .selectable_label(tool == active, format!("{} {}", spec.icon, spec.label))
                    .on_hover_text(spec.summary);
                if response.clicked() && tool != active {
                    actions.push(InspectorAction::SelectTool(tool));
                }
            }
        });
    }

    fn ui_inspector_tool_header(
        &mut self,
        ui: &mut egui::Ui,
        tab_idx: usize,
        actions: &mut Vec<InspectorAction>,
    ) {
        let spec = self.tabs[tab_idx].active_tool.spec();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{}  {}", spec.icon, spec.label))
                    .strong()
                    .size(15.0),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new("?").weak()).on_hover_text(spec.help);
                if ui
                    .small_button("⟲")
                    .on_hover_text("Reset this tool's settings")
                    .clicked()
                {
                    actions.push(InspectorAction::ResetSettings);
                }
            });
        });
        widgets::note(ui, spec.summary);
    }

    /// What the tool will act on: the span, the channels, the band.
    fn ui_inspector_target(
        &mut self,
        ui: &mut egui::Ui,
        tab_idx: usize,
        actions: &mut Vec<InspectorAction>,
    ) {
        let tab = &self.tabs[tab_idx];
        let tool = tab.active_tool;
        let spec = tool.spec();
        if spec.target == TargetRule::None {
            return;
        }
        let target = self.tool_target_for_tab(tab_idx, tool);
        let sr = tab.buffer_sample_rate.max(1) as f32;
        let fmt = |samples: usize| crate::app::helpers::format_time_s(samples as f32 / sr);
        let has_selection = Self::editor_selected_range(tab).is_some();
        let extra = tab.extra_selections.len();
        let channel_text = if spec.changes_length {
            "All channels (the length changes)".to_string()
        } else if spec.honors_channel_view {
            Self::editor_channel_mask_label(tab).unwrap_or_else(|| "All channels".to_string())
        } else {
            "All channels".to_string()
        };
        let band = tab.freq_selection;
        let insert_from_selection = has_selection;
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::symmetric(8, 4))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Target").weak().small());
                    let text = match target {
                        RenderTarget::Range(s, e) => {
                            let more = if extra > 0 && tool == ToolKind::Trim {
                                format!("  +{extra}")
                            } else {
                                String::new()
                            };
                            format!("Selection {}–{} ({}){more}", fmt(s), fmt(e), fmt(e - s))
                        }
                        RenderTarget::Whole => {
                            if spec.target == TargetRule::SelectionOnly {
                                String::new()
                            } else {
                                format!("Whole file ({})", fmt(tab.samples_len))
                            }
                        }
                        RenderTarget::InsertAt(p) => format!(
                            "Insert at {} ({})",
                            fmt(p),
                            if insert_from_selection {
                                "selection start"
                            } else {
                                "playhead"
                            }
                        ),
                    };
                    if spec.target == TargetRule::SelectionOnly && !has_selection {
                        widgets::warn(ui, "Select a range on the waveform");
                    } else {
                        ui.add(
                            egui::Label::new(RichText::new(text).monospace().small())
                                .truncate()
                                .show_tooltip_when_elided(true),
                        );
                    }
                    if matches!(target, RenderTarget::Range(..)) {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .small_button("▶")
                                .on_hover_text("Play only this range (and only the band, when one is selected)")
                                .clicked()
                            {
                                actions.push(InspectorAction::PlayTarget);
                            }
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new(channel_text).weak().small());
                    if spec.target == TargetRule::WholeOnly && has_selection {
                        ui.label(RichText::new("· the selection is not used").weak().small());
                    }
                    if let Some((lo, hi)) = band {
                        ui.label(RichText::new(format!("· band {lo:.0}–{hi:.0} Hz")).weak().small());
                    }
                });
            });
    }

    /// The tool's own settings, with the draft history kept for them.
    fn ui_inspector_tool_panel(
        &mut self,
        ui: &mut egui::Ui,
        tab_idx: usize,
        actions: &mut Vec<InspectorAction>,
    ) {
        let before = DraftSnapshot::capture(&self.tabs[tab_idx]);
        let mut g = Gestures::default();
        tools::panel(self, ui, tab_idx, &mut g, actions);
        let pointer_down = ui.input(|i| i.pointer.primary_down());
        let focused = ui.ctx().memory(|m| m.focused().is_some());
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        let after = DraftSnapshot::capture(tab);
        let now = Instant::now();
        let session = &mut tab.tool_session;
        if g.began {
            session.begin_gesture(before.clone());
        }
        if after != before {
            if session.gesture_open.is_some() {
                session.note_live_change(now);
            } else {
                session.note_change(before, &after, now);
            }
        }
        // A plot drag reports values, not drag ends: a gesture still open
        // once the pointer is up and nothing has focus is over.
        if g.ended || (session.gesture_open.is_some() && !pointer_down && !focused && !g.began) {
            session.end_gesture(&after, now);
        }
    }

    fn ui_inspector_footer(
        &mut self,
        ui: &mut egui::Ui,
        tab_idx: usize,
        actions: &mut Vec<InspectorAction>,
    ) {
        let applying = self.editor_apply_busy_for_tab(tab_idx);
        let can_apply = self.active_tool_can_apply(tab_idx);
        let tab = &self.tabs[tab_idx];
        let tool = tab.active_tool;
        let spec = tool.spec();
        let session = &tab.tool_session;
        let busy = self.tool_preview_busy_for_tab(tab_idx);
        let preview_target = (spec.preview == PreviewKind::Rendered)
            .then(|| self.tool_preview_target(tab_idx))
            .flatten();
        let fresh = preview_target
            .as_ref()
            .is_some_and(|(key, _)| session.fresh_preview(key).is_some());
        let live_on = tab.preview_audio_tool == Some(ToolKind::Panner);
        let pencil_editing = tab.pencil_draft.is_some();
        let recently_applied = session
            .applied_at
            .is_some_and(|at| at.elapsed() < APPLIED_NOTICE);
        let has_draft = session.phase == SessionPhase::Previewing
            || session.preview.is_some()
            || session.has_draft_history(false)
            || pencil_editing
            || live_on;
        // The status line.
        ui.horizontal(|ui| {
            let dot = |ui: &mut egui::Ui, color: Color32| {
                ui.label(RichText::new("●").color(color));
            };
            if applying {
                ui.add(egui::Spinner::new().size(12.0));
                let msg = self
                    .editor_apply_state
                    .as_ref()
                    .map(|state| state.msg.clone())
                    .unwrap_or_else(|| "Applying...".to_string());
                ui.label(RichText::new(msg).small());
                if ui.small_button("Cancel").clicked() {
                    actions.push(InspectorAction::CancelApply);
                }
                return;
            }
            match spec.preview {
                PreviewKind::Rendered => {
                    if let Some(err) = session.error.as_ref() {
                        dot(ui, DOT_ERROR);
                        ui.label(RichText::new(err).small().color(DOT_ERROR));
                    } else if session.phase == SessionPhase::Idle {
                        if recently_applied {
                            dot(ui, DOT_LIVE);
                            ui.label(RichText::new("Applied — Ctrl+Z to undo").small());
                        } else {
                            dot(ui, DOT_OFF);
                            ui.label(RichText::new("Hearing the original").small().weak());
                        }
                    } else if preview_target.is_none() {
                        dot(ui, DOT_OFF);
                        ui.label(RichText::new("No change to hear").small().weak());
                    } else if busy {
                        ui.add(egui::Spinner::new().size(12.0));
                        ui.label(RichText::new("Rendering preview...").small());
                    } else if fresh {
                        if session.ab_original {
                            dot(ui, DOT_AB);
                            ui.label(RichText::new("Hearing the original (A/B)").small());
                        } else {
                            dot(ui, DOT_LIVE);
                            ui.label(RichText::new("Hearing the preview").small());
                        }
                    } else if session.manual {
                        dot(ui, DOT_PENDING);
                        ui.label(RichText::new("Settings changed — Refresh to hear them").small());
                    } else {
                        dot(ui, DOT_PENDING);
                        ui.label(RichText::new("Updating...").small());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let mut auto = !session.manual;
                        if ui
                            .checkbox(&mut auto, RichText::new("Auto").small())
                            .on_hover_text("Re-render the preview whenever a setting changes")
                            .changed()
                        {
                            actions.push(InspectorAction::SetManual(!auto));
                        }
                    });
                }
                PreviewKind::Own => {
                    let (busy, heard, what) = match tool {
                        ToolKind::MusicAnalyze => (
                            tab.music_analysis_draft.preview_inflight,
                            tab.music_analysis_draft.preview_active,
                            "Hearing the stem mix",
                        ),
                        _ => (
                            self.plugin_process_state
                                .as_ref()
                                .is_some_and(|s| s.tab_idx == tab_idx && !s.is_apply),
                            tab.preview_audio_tool == Some(ToolKind::PluginFx),
                            "Hearing the plugin chain",
                        ),
                    };
                    if busy {
                        ui.add(egui::Spinner::new().size(12.0));
                        ui.label(RichText::new("Rendering preview...").small());
                    } else if heard {
                        dot(ui, DOT_LIVE);
                        ui.label(RichText::new(what).small());
                    } else {
                        dot(ui, DOT_OFF);
                        ui.label(RichText::new("Hearing the original").small().weak());
                    }
                }
                PreviewKind::Live => {
                    if live_on {
                        dot(ui, DOT_LIVE);
                        ui.label(RichText::new("Live: the pan is heard as it moves").small());
                    } else {
                        dot(ui, DOT_OFF);
                        ui.label(RichText::new("Hearing the original").small().weak());
                    }
                }
                PreviewKind::Draft => {
                    if pencil_editing {
                        dot(ui, DOT_LIVE);
                        ui.label(RichText::new("Editing the draft (green)").small());
                    } else {
                        dot(ui, DOT_OFF);
                        ui.label(RichText::new("Press Edit to start a draft").small().weak());
                    }
                }
                PreviewKind::None => {
                    let text = match tool {
                        ToolKind::Markers | ToolKind::EditorNote | ToolKind::LoopEdit => {
                            "Changes are kept as you make them (Ctrl+Z undoes)"
                        }
                        _ => "Nothing to preview: Apply makes the edit",
                    };
                    ui.label(RichText::new(text).small().weak());
                }
            }
        });
        // The buttons.
        let shows_apply = !matches!(tool, ToolKind::Markers | ToolKind::EditorNote);
        ui.horizontal(|ui| {
            match spec.preview {
                PreviewKind::Rendered => {
                    let idle = session.phase == SessionPhase::Idle;
                    let stale = !fresh && !busy;
                    let label = if idle { "Preview" } else { "Refresh" };
                    if (idle || (session.manual && stale))
                        && ui
                            .add_enabled(
                                preview_target.is_some() && !applying,
                                egui::Button::new(label),
                            )
                            .on_hover_text("Hear the settings before applying them")
                            .on_disabled_hover_text("These settings change nothing")
                            .clicked()
                    {
                        actions.push(InspectorAction::Preview);
                    }
                    if fresh
                        && ui
                            .add(egui::Button::selectable(session.ab_original, "A/B"))
                            .on_hover_text("Switch between the original and the preview at the same place")
                            .clicked()
                    {
                        actions.push(InspectorAction::ToggleAb);
                    }
                }
                PreviewKind::Own => {
                    if ui
                        .add_enabled(!applying, egui::Button::new("Preview"))
                        .on_hover_text("Render the preview and hear it")
                        .clicked()
                    {
                        actions.push(InspectorAction::Preview);
                    }
                }
                PreviewKind::Live => {
                    if ui
                        .add(egui::Button::selectable(live_on, "Live preview"))
                        .on_hover_text("Play and turn the knobs: the pan is heard as it moves. Nothing is written until Apply.")
                        .clicked()
                    {
                        actions.push(InspectorAction::Tool(ToolCommand::PannerLive(!live_on)));
                    }
                }
                PreviewKind::Draft => {
                    if ui
                        .add_enabled(!pencil_editing && !applying, egui::Button::new("Edit"))
                        .clicked()
                    {
                        actions.push(InspectorAction::Tool(ToolCommand::PencilEdit));
                    }
                    if ui
                        .add_enabled(pencil_editing && !applying, egui::Button::new("Reset"))
                        .on_hover_text("Throw away the strokes and start the draft again")
                        .clicked()
                    {
                        actions.push(InspectorAction::Tool(ToolCommand::PencilReset));
                    }
                }
                PreviewKind::None => {}
            }
            if shows_apply {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let apply_label = if tool == ToolKind::Panner
                        && self.tabs[tab_idx].ch_samples.len() == 1
                    {
                        "Apply (to stereo)"
                    } else {
                        "Apply"
                    };
                    if ui
                        .add_enabled(
                            can_apply,
                            egui::Button::new(RichText::new(apply_label).strong()),
                        )
                        .on_hover_text("Write the settings into the audio (Ctrl+Enter). Ctrl+Z undoes.")
                        .clicked()
                    {
                        actions.push(InspectorAction::Apply);
                    }
                    if ui
                        .add_enabled(has_draft && !applying, egui::Button::new("Discard"))
                        .on_hover_text("Throw the adjustments away and hear the original (Esc)")
                        .clicked()
                    {
                        actions.push(InspectorAction::Discard);
                    }
                });
            }
        });
    }
}
