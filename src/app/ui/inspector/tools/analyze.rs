//! Music Analyze and Plugin FX. Both run pipelines of their own (a model
//! download and analysis; an out-of-process plugin host), so their panels
//! keep their own setup buttons. Preview and Apply are still the footer's:
//! it asks these tools' pipelines (see `inspector_ops`).

use std::path::PathBuf;

use egui::{Color32, RichText};

use crate::app::music_onnx;
use crate::app::types::{
    MusicAnalysisSourceKind, PluginFxChainDraft, PluginFxDraft, PluginFxSlot, PluginPreviewEngine,
};
use crate::app::ui::inspector::widgets;
use crate::app::WavesPreviewer;

const GOOD: Color32 = Color32::from_rgb(120, 220, 140);
const MEH: Color32 = Color32::from_rgb(220, 170, 120);

pub(super) fn music(app: &mut WavesPreviewer, ui: &mut egui::Ui, tab_idx: usize) {
    let model_ready = app.music_ai_has_model();
    let demucs_ready = app.music_ai_has_demucs_model();
    let downloading = app.music_model_download_state.is_some();
    let can_uninstall = app.music_ai_can_uninstall();
    let analyze_running = app.music_ai_state.is_some();
    let run_status = app.music_analysis_status_text();
    let run_process = app.music_analysis_process_text();
    let model_dir = app
        .music_ai_model_dir
        .as_ref()
        .map(|p| p.display().to_string());
    let global_error = app.music_ai_last_error.clone();
    let mut download = false;
    let mut uninstall = false;
    let mut analyze = false;
    let mut cancel = false;
    let mut rebuild_markers = false;
    let mut apply_markers = false;

    {
        let tab = &mut app.tabs[tab_idx];
        if model_ready {
            ui.label(RichText::new("Analyze model: ready").color(GOOD));
            ui.label(
                RichText::new(if demucs_ready {
                    "Auto Demucs: ready"
                } else {
                    "Auto Demucs: missing"
                })
                .color(if demucs_ready { GOOD } else { MEH }),
            );
        } else if downloading {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new());
                ui.label(RichText::new("Analyze model: downloading...").weak());
            });
        } else {
            ui.label(RichText::new("Analyze model: not installed").weak());
        }
        if let Some(dir) = model_dir.as_ref() {
            widgets::note(ui, &format!("Model dir: {dir}"));
        }
        ui.horizontal_wrapped(|ui| {
            if !model_ready {
                download |= ui
                    .add_enabled(!downloading, egui::Button::new("Download Model..."))
                    .clicked();
            } else {
                if !demucs_ready {
                    download |= ui
                        .add_enabled(can_uninstall, egui::Button::new("Repair Model Files..."))
                        .clicked();
                }
                uninstall |= ui
                    .add_enabled(can_uninstall, egui::Button::new("Uninstall Model..."))
                    .clicked();
            }
        });
        if let Some(err) = tab
            .music_analysis_draft
            .last_error
            .as_ref()
            .or(global_error.as_ref())
        {
            ui.label(RichText::new(err).color(Color32::LIGHT_RED));
        }
        ui.separator();
        // Resolved per frame like before; this only looks at a few file names
        // next to the source.
        let stems = music_onnx::resolve_stem_paths(
            tab.path.as_path(),
            tab.music_analysis_draft.stems_dir_override.as_deref(),
        );
        if stems.is_ready() {
            ui.label(RichText::new("Input: stems ready").color(GOOD));
            widgets::note(ui, &format!("Stem dir: {}", stems.root_dir.display()));
        } else if demucs_ready {
            ui.label(
                RichText::new("Input: source audio (auto Demucs)")
                    .color(Color32::from_rgb(120, 200, 220)),
            );
        } else {
            ui.label(RichText::new("Input unavailable: no stems and no auto-Demucs").color(MEH));
            widgets::note(ui, &format!("Missing: {}", stems.missing.join(", ")));
        }
        if music_onnx::source_audio_has_timing_risk(tab.path.as_path()) {
            widgets::warn(
                ui,
                "Compressed input can shift beat markers; WAV is recommended.",
            );
        }
        let can_analyze = model_ready
            && (stems.is_ready() || demucs_ready)
            && !tab.music_analysis_draft.analysis_inflight
            && !analyze_running;
        ui.horizontal_wrapped(|ui| {
            analyze |= ui
                .add_enabled(can_analyze, egui::Button::new("Analyze"))
                .clicked();
            cancel |= ui
                .add_enabled(
                    tab.music_analysis_draft.analysis_inflight || analyze_running,
                    egui::Button::new("Stop"),
                )
                .clicked();
            if let Some(status) = run_status.as_ref() {
                ui.label(RichText::new(status).weak());
            }
        });
        if let Some(process) = run_process.as_ref() {
            ui.horizontal_wrapped(|ui| {
                if analyze_running {
                    ui.add(egui::Spinner::new());
                }
                widgets::note(ui, &format!("Process: {process}"));
            });
        }
        let Some(result) = tab.music_analysis_draft.result.as_ref() else {
            widgets::note(
                ui,
                if analyze_running {
                    "Analyze is running; the markers and stem sliders appear when it finishes."
                } else {
                    "Run Analyze to get beat / section markers and the stem sliders."
                },
            );
            run_music_commands(
                app, tab_idx, download, uninstall, analyze, cancel, false, false,
            );
            return;
        };
        widgets::note(
            ui,
            &format!(
                "beats {} · downbeats {} · sections {}{} · from {}",
                result.beats.len(),
                result.downbeats.len(),
                result.sections.len(),
                result
                    .estimated_bpm
                    .map(|v| format!(" · {v:.2} BPM"))
                    .unwrap_or_default(),
                match tab.music_analysis_draft.analysis_source_kind {
                    MusicAnalysisSourceKind::StemsDir => "stems",
                    MusicAnalysisSourceKind::AutoDemucs => "auto-Demucs",
                }
            ),
        );
        widgets::section(ui, "Markers");
        let draft = &mut tab.music_analysis_draft;
        ui.horizontal_wrapped(|ui| {
            rebuild_markers |= ui.checkbox(&mut draft.show_beat, "Beat").changed();
            rebuild_markers |= ui.checkbox(&mut draft.show_downbeat, "Downbeat").changed();
            rebuild_markers |= ui.checkbox(&mut draft.show_section, "Section").changed();
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!(
                    "{} marker(s) shown",
                    draft.provisional_markers.len()
                ))
                .weak(),
            );
            apply_markers |= ui
                .button("Add as markers")
                .on_hover_text("Turn the shown beats / sections into the file's markers (undoable)")
                .clicked();
        });
        widgets::section(ui, "Stem mix (the footer previews and applies it)");
        let mut changed = false;
        changed |= ui
            .checkbox(&mut draft.preview_click_beat, "Beat click")
            .changed();
        changed |= ui
            .checkbox(&mut draft.preview_click_downbeat, "Downbeat accent")
            .changed();
        changed |= ui
            .checkbox(&mut draft.preview_cue_section, "Section cue")
            .changed();
        let range = crate::app::helpers::GAIN_DB_MIN..=crate::app::helpers::GAIN_DB_MAX;
        for (label, value) in [
            ("bass", &mut draft.preview_gains_db.bass),
            ("drums", &mut draft.preview_gains_db.drums),
            ("other", &mut draft.preview_gains_db.other),
            ("vocals", &mut draft.preview_gains_db.vocals),
        ] {
            changed |= ui
                .add(
                    egui::Slider::new(value, range.clone())
                        .text(label)
                        .suffix(" dB"),
                )
                .changed();
            *value = value.clamp(*range.start(), *range.end());
        }
        changed |= ui
            .checkbox(&mut draft.preview_selection_only, "Selection only")
            .changed();
        if changed {
            draft.preview_active = false;
        }
        widgets::note(
            ui,
            &format!(
                "Peak {:.4}{}",
                draft.preview_peak_abs,
                if draft.preview_clip_applied {
                    " (clipped)"
                } else {
                    ""
                }
            ),
        );
        if let Some(err) = draft.preview_error.as_ref() {
            ui.label(RichText::new(err).small().color(Color32::LIGHT_RED));
        }
    }
    run_music_commands(
        app,
        tab_idx,
        download,
        uninstall,
        analyze,
        cancel,
        rebuild_markers,
        apply_markers,
    );
}

#[allow(clippy::too_many_arguments)]
fn run_music_commands(
    app: &mut WavesPreviewer,
    tab_idx: usize,
    download: bool,
    uninstall: bool,
    analyze: bool,
    cancel: bool,
    rebuild_markers: bool,
    apply_markers: bool,
) {
    if download {
        app.queue_music_model_download();
    }
    if uninstall {
        app.uninstall_music_model_cache();
    }
    if analyze {
        app.start_music_analysis_for_tab(tab_idx);
    }
    if cancel {
        app.cancel_music_analysis_run();
    }
    if rebuild_markers {
        app.rebuild_music_provisional_markers_for_tab(tab_idx);
    }
    if apply_markers {
        app.apply_music_markers_with_undo(tab_idx);
    }
}

pub(super) fn plugin_fx(app: &mut WavesPreviewer, ui: &mut egui::Ui, tab_idx: usize) {
    let plugin_catalog = app.plugin_catalog.clone();
    let plugin_search_paths = app.plugin_search_paths.clone();
    let mut search_path_input = std::mem::take(&mut app.plugin_search_path_input);
    let mut preset_name_input = std::mem::take(&mut app.plugin_preset_name_input);
    let scan_busy = app.plugin_scan_state.is_some();
    let scan_error = app.plugin_scan_error.clone();
    let tab_path = app.tabs[tab_idx].path.clone();
    let probe_busy = app
        .plugin_probe_state
        .as_ref()
        .is_some_and(|s| s.tab_path == tab_path);

    let mut scan = false;
    let mut pick_folder = false;
    let mut load_from_file = false;
    let mut reset_paths = false;
    let mut remove_path: Option<usize> = None;
    let mut add_path: Option<PathBuf> = None;
    let mut probe: Option<String> = None;
    let mut gui_open = false;
    let mut gui_sync = false;
    let mut gui_close = false;
    let mut restore_preview = false;

    {
        let tab = &mut app.tabs[tab_idx];
        if scan_busy {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new());
                ui.label(RichText::new("Scanning plugins...").weak());
            });
        } else if let Some(err) = scan_error.as_ref() {
            ui.label(RichText::new(format!("Plugin scan failed: {err}")).color(Color32::LIGHT_RED));
        }
        if tab.plugin_fx_chain.slots.is_empty() && tab.plugin_fx_draft.plugin_key.is_some() {
            tab.plugin_fx_chain = PluginFxChainDraft::from_legacy(&tab.plugin_fx_draft);
        }
        let mut remove_slot = None;
        let mut move_slot = None;
        widgets::section(ui, "Chain");
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut tab.plugin_fx_chain.bypass, "Bypass chain");
            widgets::note(
                ui,
                &format!(
                    "latency {} smp · underruns {}",
                    tab.plugin_fx_chain.latency_samples, tab.plugin_fx_chain.underrun_count
                ),
            );
        });
        let slot_count = tab.plugin_fx_chain.slots.len();
        for index in 0..slot_count {
            let slot_id = tab.plugin_fx_chain.slots[index].id;
            let selected = tab.plugin_fx_chain.selected_slot_id == Some(slot_id);
            ui.horizontal_wrapped(|ui| {
                let slot = &mut tab.plugin_fx_chain.slots[index];
                ui.checkbox(&mut slot.draft.enabled, "");
                let label = if slot.draft.plugin_name.is_empty() {
                    format!("{}  (select a plugin)", index + 1)
                } else {
                    format!("{}  {}", index + 1, slot.draft.plugin_name)
                };
                if ui.selectable_label(selected, label).clicked() {
                    tab.plugin_fx_chain.selected_slot_id = Some(slot_id);
                }
                let slot = &mut tab.plugin_fx_chain.slots[index];
                ui.checkbox(&mut slot.draft.bypass, "Bypass");
                if ui.small_button("↑").clicked() && index > 0 {
                    move_slot = Some((index, index - 1));
                }
                if ui.small_button("↓").clicked() && index + 1 < slot_count {
                    move_slot = Some((index, index + 1));
                }
                if ui
                    .small_button("✕")
                    .on_hover_text("Remove this slot")
                    .clicked()
                {
                    remove_slot = Some(slot_id);
                }
                if let Some(reason) = slot.failure_reason.as_deref() {
                    ui.label(RichText::new(reason).small().color(Color32::LIGHT_RED));
                }
            });
        }
        if ui.button("+ Add slot").clicked() {
            let slot = PluginFxSlot::new(PluginFxDraft::default());
            tab.plugin_fx_chain.selected_slot_id = Some(slot.id);
            tab.plugin_fx_chain.slots.push(slot);
        }
        if let Some((from, to)) = move_slot {
            tab.plugin_fx_chain.slots.swap(from, to);
        }
        if let Some(slot_id) = remove_slot {
            tab.plugin_fx_chain.slots.retain(|slot| slot.id != slot_id);
            if tab.plugin_fx_chain.selected_slot_id == Some(slot_id) {
                tab.plugin_fx_chain.selected_slot_id = None;
            }
        }
        if tab.plugin_fx_chain.selected_slot_id.is_none() {
            tab.plugin_fx_chain.selected_slot_id =
                tab.plugin_fx_chain.slots.first().map(|slot| slot.id);
        }
        tab.plugin_fx_draft = tab
            .plugin_fx_chain
            .selected_slot_id
            .and_then(|id| tab.plugin_fx_chain.slots.iter().find(|slot| slot.id == id))
            .map(|slot| slot.draft.clone())
            .unwrap_or_default();
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Preview engine").small());
            ui.selectable_value(
                &mut tab.plugin_fx_chain.preview_engine,
                PluginPreviewEngine::RenderAhead,
                "Render-ahead",
            );
            ui.selectable_value(
                &mut tab.plugin_fx_chain.preview_engine,
                PluginPreviewEngine::Offline,
                "Offline",
            );
            ui.add_enabled(
                tab.plugin_fx_chain.preview_engine == PluginPreviewEngine::RenderAhead,
                egui::Slider::new(&mut tab.plugin_fx_chain.render_ahead_ms, 100..=500)
                    .suffix(" ms"),
            );
        });

        widgets::section(ui, "Selected slot");
        let mut params_dirty = false;
        let draft = &mut tab.plugin_fx_draft;
        let selected_text = draft
            .plugin_key
            .as_deref()
            .and_then(|key| {
                plugin_catalog.iter().find(|e| e.key == key).map(|e| {
                    format!(
                        "{:?}: {}",
                        e.format,
                        WavesPreviewer::plugin_path_label(&e.path)
                    )
                })
            })
            .or_else(|| (!draft.plugin_name.is_empty()).then(|| draft.plugin_name.clone()))
            .unwrap_or_else(|| "(select a plugin)".to_string());
        let mut selected_changed = false;
        egui::ComboBox::from_id_salt(("plugin_fx_select", tab_idx))
            .selected_text(selected_text)
            .width(ui.available_width() - 8.0)
            .show_ui(ui, |ui| {
                for entry in plugin_catalog.iter() {
                    let selected = draft.plugin_key.as_deref() == Some(entry.key.as_str());
                    let label = format!(
                        "{:?}: {}",
                        entry.format,
                        WavesPreviewer::plugin_path_label(&entry.path)
                    );
                    if ui.selectable_label(selected, label).clicked() {
                        draft.plugin_key = Some(entry.key.clone());
                        draft.plugin_name = entry.name.clone();
                        draft.params.clear();
                        draft.last_error = None;
                        draft.last_backend_log = None;
                        probe = Some(entry.key.clone());
                        selected_changed = true;
                    }
                }
            });
        ui.horizontal_wrapped(|ui| {
            scan |= ui.button("Rescan").clicked();
            let gui_live = draft.gui_status == crate::plugin::GuiSessionStatus::Live;
            let can_reload = draft.plugin_key.is_some() && !probe_busy && !scan_busy;
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
                    gui_sync = true;
                } else {
                    probe = draft.plugin_key.clone();
                    draft.last_backend_log = None;
                }
            }
            gui_open |= ui
                .add_enabled(
                    can_reload && draft.gui_capabilities.supports_native_gui,
                    egui::Button::new("Open GUI"),
                )
                .clicked();
            gui_close |= ui
                .add_enabled(gui_live, egui::Button::new("Close GUI"))
                .clicked();
            load_from_file |= ui
                .button("Load from file...")
                .on_hover_text("Pick a .vst3 / .clap plugin directly, without a Rescan")
                .clicked();
        });
        ui.collapsing("Search paths", |ui| {
            ui.horizontal_wrapped(|ui| {
                pick_folder |= ui.button("Add folder...").clicked();
                reset_paths |= ui.button("Reset defaults").clicked();
                scan |= ui.button("Rescan paths").clicked();
            });
            ui.horizontal_wrapped(|ui| {
                let edit = ui.text_edit_singleline(&mut search_path_input);
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (enter || ui.button("Add path").clicked())
                    && !search_path_input.trim().is_empty()
                {
                    add_path = Some(PathBuf::from(search_path_input.trim()));
                    search_path_input.clear();
                }
            });
            if plugin_search_paths.is_empty() {
                ui.label(RichText::new("(no search paths)").weak());
            }
            for (idx, path) in plugin_search_paths.iter().enumerate() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(path.display().to_string())
                            .small()
                            .monospace(),
                    );
                    if ui.small_button("✕").clicked() {
                        remove_path = Some(idx);
                    }
                });
            }
        });
        if selected_changed {
            restore_preview = true;
            gui_close = true;
        }
        ui.horizontal_wrapped(|ui| {
            params_dirty |= ui.checkbox(&mut draft.enabled, "Enable").changed();
            params_dirty |= ui.checkbox(&mut draft.bypass, "Bypass").changed();
            if let Some(backend) = draft.backend {
                widgets::note(
                    ui,
                    &format!("backend {backend:?} · GUI {:?}", draft.gui_status),
                );
            }
        });
        if let Some(plugin_key) = draft.plugin_key.clone() {
            let mut load_preset: Option<PathBuf> = None;
            let mut delete_preset: Option<PathBuf> = None;
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt(("plugin_fx_preset", tab_idx))
                    .selected_text("Load preset...")
                    .show_ui(ui, |ui| {
                        // Listed only while the menu is open: the listing
                        // reads the preset folder.
                        let presets = WavesPreviewer::list_plugin_presets_for_key(&plugin_key);
                        if presets.is_empty() {
                            ui.label(RichText::new("(no presets saved)").weak());
                        }
                        for (name, path) in presets {
                            ui.horizontal(|ui| {
                                if ui.selectable_label(false, &name).clicked() {
                                    load_preset = Some(path.clone());
                                }
                                if ui.small_button("Delete").clicked() {
                                    delete_preset = Some(path.clone());
                                }
                            });
                        }
                    });
                ui.add(
                    egui::TextEdit::singleline(&mut preset_name_input)
                        .hint_text("Preset name")
                        .desired_width(110.0),
                );
                let can_save = !preset_name_input.trim().is_empty() && !draft.params.is_empty();
                if ui
                    .add_enabled(can_save, egui::Button::new("Save"))
                    .clicked()
                {
                    use base64::Engine as _;
                    let params =
                        crate::app::plugin_preset_ops::preset_params_from_ui(&draft.params);
                    let blob_b64 = draft
                        .state_blob
                        .as_ref()
                        .map(|b| base64::engine::general_purpose::STANDARD_NO_PAD.encode(b));
                    match WavesPreviewer::save_plugin_preset_for_key(
                        &plugin_key,
                        preset_name_input.trim(),
                        params,
                        blob_b64,
                    ) {
                        Ok(_) => {
                            preset_name_input.clear();
                            draft.last_error = None;
                        }
                        Err(err) => draft.last_error = Some(err),
                    }
                }
            });
            if let Some(path) = load_preset {
                use base64::Engine as _;
                match crate::app::plugin_preset_ops::load_plugin_preset_from(&path) {
                    Ok(preset) => {
                        draft.params =
                            crate::app::plugin_preset_ops::preset_params_to_ui(&preset.params);
                        draft.state_blob = preset.state_blob_b64.as_ref().and_then(|raw| {
                            base64::engine::general_purpose::STANDARD_NO_PAD
                                .decode(raw.as_bytes())
                                .ok()
                        });
                        draft.last_error = None;
                        restore_preview = true;
                        params_dirty = true;
                    }
                    Err(err) => draft.last_error = Some(err),
                }
            }
            if let Some(path) = delete_preset {
                if let Err(err) = crate::app::plugin_preset_ops::delete_plugin_preset_file(&path) {
                    draft.last_error = Some(err);
                }
            }
            ui.horizontal_wrapped(|ui| {
                if draft.ab_alt.is_none() {
                    if ui
                        .button("Store B")
                        .on_hover_text(
                            "Copy these settings into slot B, tweak, then Swap to compare",
                        )
                        .clicked()
                    {
                        draft.ab_alt = Some((draft.params.clone(), draft.state_blob.clone()));
                        draft.ab_active_b = false;
                    }
                } else {
                    if ui.button("Swap A/B settings").clicked() {
                        if let Some((alt_params, alt_blob)) = draft.ab_alt.take() {
                            let cur = (
                                std::mem::replace(&mut draft.params, alt_params),
                                std::mem::replace(&mut draft.state_blob, alt_blob),
                            );
                            draft.ab_alt = Some(cur);
                            draft.ab_active_b = !draft.ab_active_b;
                            restore_preview = true;
                            params_dirty = true;
                        }
                    }
                    widgets::note(
                        ui,
                        if draft.ab_active_b {
                            "Active: B"
                        } else {
                            "Active: A"
                        },
                    );
                    if ui.small_button("Clear B").clicked() {
                        draft.ab_alt = None;
                        draft.ab_active_b = false;
                    }
                }
            });
        }
        if !draft.gui_capabilities.supports_native_gui {
            widgets::note(ui, "This plugin / backend has no native GUI here.");
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Filter");
            ui.text_edit_singleline(&mut draft.filter);
        });
        if probe_busy {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new());
                ui.label(RichText::new("Loading parameters...").weak());
            });
        }
        let filter = draft.filter.trim().to_ascii_lowercase();
        if draft.params.is_empty() {
            ui.label(RichText::new("No parameters").weak());
        }
        for param in draft.params.iter_mut() {
            if !filter.is_empty()
                && !param.name.to_ascii_lowercase().contains(&filter)
                && !param.id.to_ascii_lowercase().contains(&filter)
            {
                continue;
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new(param.name.as_str()).monospace());
                let mut norm = param.normalized;
                if ui
                    .add(egui::Slider::new(&mut norm, 0.0..=1.0).show_value(false))
                    .changed()
                {
                    param.normalized = norm.clamp(0.0, 1.0);
                    params_dirty = true;
                }
                let actual = param.min + (param.max - param.min) * param.normalized;
                ui.label(RichText::new(format!("{actual:.3}{}", param.unit)).small());
                if ui.small_button("⟲").on_hover_text("Default").clicked() {
                    param.normalized = param.default_normalized.clamp(0.0, 1.0);
                    params_dirty = true;
                }
            });
        }
        WavesPreviewer::ui_plugin_probe_status(
            ui,
            draft.last_error.as_deref(),
            draft.last_backend_note.as_deref(),
            draft.last_backend_log.as_deref(),
        );
        let updated = draft.clone();
        if let Some(slot) = tab.plugin_fx_chain.selected_slot_id.and_then(|id| {
            tab.plugin_fx_chain
                .slots
                .iter_mut()
                .find(|slot| slot.id == id)
        }) {
            slot.draft = updated;
        }
        if params_dirty {
            // The footer's auto preview follows plugin parameters as it does
            // any other tool's settings.
            tab.plugin_fx_param_dirty_at = Some(std::time::Instant::now());
            tab.tool_session.note_live_change(std::time::Instant::now());
        }
    }
    app.plugin_search_path_input = search_path_input;
    app.plugin_preset_name_input = preset_name_input;
    if scan {
        app.spawn_plugin_scan();
    }
    if pick_folder {
        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
            add_path = Some(folder);
        }
    }
    if load_from_file {
        if let Some(path) = app.pick_plugin_file_dialog() {
            if let Some(entry) = app.add_plugin_catalog_entry_from_path(path.clone()) {
                if let Some(tab) = app.tabs.get_mut(tab_idx) {
                    let draft = &mut tab.plugin_fx_draft;
                    draft.plugin_key = Some(entry.key.clone());
                    draft.plugin_name = entry.name;
                    draft.params.clear();
                    draft.last_error = None;
                    draft.last_backend_log = None;
                }
                app.spawn_plugin_probe_for_tab(tab_idx, entry.key);
            } else if let Some(tab) = app.tabs.get_mut(tab_idx) {
                tab.plugin_fx_draft.last_error = Some(format!(
                    "not a recognized VST3/CLAP plugin: {}",
                    path.display()
                ));
            }
        }
    }
    if reset_paths {
        app.reset_plugin_search_paths_to_default();
        app.save_prefs();
        app.plugin_catalog.clear();
        app.spawn_plugin_scan();
    }
    if let Some(index) = remove_path {
        if app.remove_plugin_search_path_at(index) {
            app.save_prefs();
            app.plugin_catalog.clear();
            app.spawn_plugin_scan();
        }
    }
    if let Some(path) = add_path {
        if app.add_plugin_search_path(path) {
            app.save_prefs();
            app.plugin_catalog.clear();
            app.spawn_plugin_scan();
        }
    }
    if let Some(key) = probe {
        app.spawn_plugin_probe_for_tab(tab_idx, key);
    }
    if gui_open {
        app.open_plugin_gui_for_tab(tab_idx);
    }
    if gui_sync {
        app.sync_plugin_gui_for_tab(tab_idx);
    }
    if gui_close {
        app.close_plugin_gui_for_tab(tab_idx);
    }
    if restore_preview {
        app.clear_preview_if_any(tab_idx);
    }
}
