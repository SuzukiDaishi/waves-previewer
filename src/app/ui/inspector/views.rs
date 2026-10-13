//! The Inspector of the analysis views (Tempogram, Chromagram, World).
//! These are not tools on a selection but readouts of an analysis, so they
//! keep their own buttons; the World view's Resynthesize is its apply.

use egui::{Color32, RichText};

use crate::app::inspector_ops::InspectorAction;
use crate::app::types::{
    EditorAnalysisKey, EditorAnalysisKind, EditorFeatureAnalysisData, EditorTab, ViewMode,
    WorldF0Method,
};
use crate::app::ui::inspector::widgets;
use crate::app::WavesPreviewer;

const READOUT: Color32 = Color32::from_rgb(120, 220, 160);
const EDITED: Color32 = Color32::from_rgb(255, 176, 64);

impl WavesPreviewer {
    pub(in crate::app) fn ui_inspector_analysis_view(
        &mut self,
        ui: &mut egui::Ui,
        tab_idx: usize,
        view: ViewMode,
        _actions: &mut Vec<InspectorAction>,
    ) {
        let kind = match view {
            ViewMode::Tempogram => EditorAnalysisKind::Tempogram,
            ViewMode::Chromagram => EditorAnalysisKind::Chromagram,
            ViewMode::World => EditorAnalysisKind::World,
            _ => return,
        };
        let key = EditorAnalysisKey {
            path: self.tabs[tab_idx].path.clone(),
            kind,
        };
        let data = self.editor_feature_cache.get(&key).cloned();
        let loading = self.editor_feature_inflight.contains(&key);
        let mut refresh = false;
        let mut cancel = false;
        let mut resynth = false;
        let mut apply_bpm: Option<f32> = None;
        let mut f0_method: Option<WorldF0Method> = None;
        let world_f0_method = self.world_f0_method;
        let mut world_ap_slider = self.world_ap_slider;
        {
            let tab = &mut self.tabs[tab_idx];
            widgets::section(
                ui,
                match view {
                    ViewMode::Tempogram => "Tempogram",
                    ViewMode::Chromagram => "Chromagram",
                    _ => "WORLD (F0 / spectral envelope)",
                },
            );
            ui.checkbox(&mut tab.show_waveform_overlay, "Waveform overlay");
            ui.horizontal_wrapped(|ui| {
                let label = match view {
                    ViewMode::Tempogram => "Analyze BPM",
                    ViewMode::Chromagram => "Analyze Key",
                    _ => "Re-analyze",
                };
                refresh |= ui.button(label).clicked();
                if loading {
                    cancel |= ui.button("Stop").clicked();
                }
            });
            match (view, data.as_deref()) {
                (ViewMode::Tempogram, Some(EditorFeatureAnalysisData::Tempogram(d))) => {
                    match d.estimated_bpm {
                        Some(bpm) => {
                            ui.label(
                                RichText::new(format!("≈ {bpm:.1} BPM"))
                                    .monospace()
                                    .size(18.0)
                                    .color(READOUT),
                            );
                            WavesPreviewer::inspector_confidence_bar(ui, d.confidence);
                            ui.horizontal_wrapped(|ui| {
                                if ui
                                    .button("Use this BPM")
                                    .on_hover_text("Set the tab's BPM (written into the file's metadata on save)")
                                    .clicked()
                                {
                                    apply_bpm = Some(bpm);
                                }
                                widgets::note(ui, &format!("½ {:.1} / ×2 {:.1}", bpm * 0.5, bpm * 2.0));
                            });
                        }
                        None => widgets::note(ui, "No stable tempo found."),
                    }
                    widgets::note(
                        ui,
                        "Bright ridge = rhythmic period. The green line marks the estimate.",
                    );
                }
                (ViewMode::Chromagram, Some(EditorFeatureAnalysisData::Chromagram(d))) => {
                    match d.estimated_key.as_deref() {
                        Some(key_name) => {
                            ui.label(
                                RichText::new(format!(
                                    "{key_name} {}",
                                    d.estimated_mode.as_deref().unwrap_or("")
                                ))
                                .monospace()
                                .size(18.0)
                                .color(READOUT),
                            );
                            WavesPreviewer::inspector_confidence_bar(ui, d.confidence);
                        }
                        None => widgets::note(ui, "No stable key found."),
                    }
                    widgets::note(
                        ui,
                        "Rows are pitch classes; bright cells show which notes sound.",
                    );
                }
                (ViewMode::World, Some(EditorFeatureAnalysisData::World(d))) => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("F0 estimator");
                        let mut method = world_f0_method;
                        egui::ComboBox::from_id_salt("world_f0_method")
                            .selected_text(match method {
                                WorldF0Method::Dio => "DIO (fast)",
                                WorldF0Method::Harvest => "Harvest (accurate)",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut method, WorldF0Method::Dio, "DIO (fast)");
                                ui.selectable_value(
                                    &mut method,
                                    WorldF0Method::Harvest,
                                    "Harvest (accurate)",
                                )
                                .on_hover_text(
                                    "More accurate pitch, several times slower to analyze",
                                );
                            });
                        if method != world_f0_method {
                            f0_method = Some(method);
                        }
                    });
                    match d.median_f0 {
                        Some(f0) => {
                            ui.label(
                                RichText::new(format!("median F0 ≈ {f0:.1} Hz"))
                                    .monospace()
                                    .size(18.0)
                                    .color(Color32::from_rgb(80, 230, 255)),
                            );
                        }
                        None => widgets::note(ui, "No voiced frames found."),
                    }
                    widgets::note(
                        ui,
                        &format!(
                            "voiced {:.0}% · {} frames · hop {:.1} ms",
                            d.voiced_ratio * 100.0,
                            d.frames,
                            d.frame_step as f32 * 1_000.0 / d.sample_rate.max(1) as f32
                        ),
                    );
                    world_edit_panel(ui, tab, d, &mut world_ap_slider);
                    let can_resynth = d.frames > 0 && d.aperiodicity.len() == d.frames * d.bins;
                    resynth = ui
                        .add_enabled(can_resynth && !loading, egui::Button::new(RichText::new("Resynthesize (replace audio)").strong()))
                        .on_hover_text("WORLD synthesis with the edits replaces this tab's audio on every channel (Ctrl+Z undoes)")
                        .on_disabled_hover_text("Re-analyze first so the aperiodicity is available")
                        .clicked();
                }
                _ if loading => widgets::note(ui, "Analyzing the mono mixdown..."),
                (ViewMode::World, _) => {
                    widgets::note(ui, "The WORLD analysis starts by itself in this view.")
                }
                _ => widgets::note(ui, "Press the button above to analyze."),
            }
        }
        self.world_ap_slider = world_ap_slider;
        if cancel || refresh {
            self.cancel_feature_analysis_for_key(&key);
        }
        if resynth {
            self.spawn_world_resynth_for_tab(tab_idx);
        }
        if let Some(method) = f0_method {
            self.world_f0_method = method;
            self.save_prefs();
            // Cached analyses were made with the previous estimator.
            let world_keys: Vec<EditorAnalysisKey> = self
                .editor_feature_cache
                .keys()
                .chain(self.editor_feature_inflight.iter())
                .filter(|k| k.kind == EditorAnalysisKind::World)
                .cloned()
                .collect();
            for k in world_keys {
                self.cancel_feature_analysis_for_key(&k);
            }
        }
        if let Some(bpm) = apply_bpm {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                tab.bpm_value = bpm;
                tab.bpm_user_set = true;
            }
        }
    }
}

fn world_edit_panel(
    ui: &mut egui::Ui,
    tab: &mut EditorTab,
    data: &crate::app::types::WorldFeatureData,
    ap_slider: &mut f32,
) {
    widgets::section(ui, "F0 edit");
    let mut edit_on = tab
        .world_f0_draft
        .as_ref()
        .is_some_and(|draft| draft.edit_enabled);
    if ui
        .checkbox(&mut edit_on, "Edit F0 on the canvas")
        .on_hover_text("Left-drag draws pitch, right-drag erases (unvoiced). Seeking and selecting pause while on.")
        .changed()
    {
        WavesPreviewer::world_f0_draft_mut(tab, data).edit_enabled = edit_on;
    }
    ui.checkbox(&mut tab.world_f0_focus, "F0 zoom (50 Hz – 1.1 kHz)");
    let mut shift = tab
        .world_f0_draft
        .as_ref()
        .map_or(0.0, |d| d.shift_semitones);
    ui.horizontal_wrapped(|ui| {
        ui.label("Shift");
        if ui
            .add(
                egui::DragValue::new(&mut shift)
                    .range(-24.0..=24.0)
                    .speed(0.1)
                    .suffix(" st"),
            )
            .changed()
        {
            WavesPreviewer::world_f0_draft_mut(tab, data).shift_semitones = shift;
        }
        if ui.button("Apply shift").clicked() && shift != 0.0 {
            let draft = WavesPreviewer::world_f0_draft_mut(tab, data);
            WavesPreviewer::world_f0_shift_semitones(draft, shift);
            draft.shift_semitones = 0.0;
        }
    });
    ui.horizontal_wrapped(|ui| {
        if ui
            .button("Smooth")
            .on_hover_text("5-frame median over voiced runs")
            .clicked()
        {
            WavesPreviewer::world_f0_smooth(WavesPreviewer::world_f0_draft_mut(tab, data));
        }
        if ui
            .button("Flatten")
            .on_hover_text("Every voiced frame at the median F0")
            .clicked()
        {
            WavesPreviewer::world_f0_flatten(WavesPreviewer::world_f0_draft_mut(tab, data));
        }
        if ui.button("Reset").clicked() {
            let draft = WavesPreviewer::world_f0_draft_mut(tab, data);
            WavesPreviewer::world_f0_reset(draft, data);
        }
    });
    if tab.world_f0_draft.as_ref().is_some_and(|d| d.dirty) {
        ui.label(
            RichText::new("F0 edited (orange curve)")
                .small()
                .color(EDITED),
        );
    }
    ui.horizontal_wrapped(|ui| {
        ui.label("Formant");
        let mut ratio = if tab.world_formant_ratio.is_finite() && tab.world_formant_ratio > 0.0 {
            tab.world_formant_ratio
        } else {
            1.0
        };
        if ui
            .add(egui::Slider::new(&mut ratio, 0.5..=2.0).logarithmic(true).fixed_decimals(2).suffix("×"))
            .on_hover_text("Warp the spectral envelope at resynthesis (>1 raises formants without changing pitch)")
            .changed()
        {
            tab.world_formant_ratio = ratio;
        }
        if ui.small_button("⟲").clicked() {
            tab.world_formant_ratio = 1.0;
        }
    });
    let frames = data.frames;
    let frame_step = data.frame_step.max(1);
    ui.horizontal_wrapped(|ui| {
        ui.label("Aperiodicity");
        if !ap_slider.is_finite() || *ap_slider < 0.0 {
            *ap_slider = 1.0;
        }
        ui.add(egui::Slider::new(ap_slider, 0.0..=2.0).fixed_decimals(2).suffix("×"))
            .on_hover_text("The breath component: 0 = fully voiced, >1 = breathier. Set All / Set Selection writes it into the draft.");
    });
    let ensure_draft = |tab: &mut EditorTab| {
        let stale = tab
            .world_ap_draft
            .as_ref()
            .map_or(true, |d| d.source_frames != frames);
        if stale {
            tab.world_ap_draft = Some(crate::app::types::WorldApDraft {
                values: vec![1.0; frames],
                source_frames: frames,
                dirty: false,
            });
        }
    };
    let ap = *ap_slider;
    ui.horizontal_wrapped(|ui| {
        if ui.small_button("Set all").clicked() && frames > 0 {
            ensure_draft(tab);
            if let Some(draft) = tab.world_ap_draft.as_mut() {
                draft.values.iter_mut().for_each(|v| *v = ap);
                draft.dirty = draft.values.iter().any(|v| (*v - 1.0).abs() >= 1e-3);
            }
        }
        let sel_frames = tab
            .selection
            .map(|(s, e)| (s / frame_step, (e / frame_step).max(s / frame_step + 1)));
        if ui
            .add_enabled(
                sel_frames.is_some() && frames > 0,
                egui::Button::new("Set selection").small(),
            )
            .clicked()
        {
            if let Some((fs, fe)) = sel_frames {
                ensure_draft(tab);
                if let Some(draft) = tab.world_ap_draft.as_mut() {
                    for v in draft
                        .values
                        .iter_mut()
                        .take(fe.min(frames))
                        .skip(fs.min(frames))
                    {
                        *v = ap;
                    }
                    draft.dirty = draft.values.iter().any(|v| (*v - 1.0).abs() >= 1e-3);
                }
            }
        }
        if ui.small_button("⟲").clicked() {
            tab.world_ap_draft = None;
        }
    });
    if tab.world_ap_draft.as_ref().is_some_and(|d| d.dirty) {
        ui.label(RichText::new("Aperiodicity edited").small().color(EDITED));
    }
}
