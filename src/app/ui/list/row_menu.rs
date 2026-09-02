use crate::app::WavesPreviewer;

impl WavesPreviewer {
    fn list_row_context_menu_contents(&mut self, ui: &mut egui::Ui) {
        let selected = self.selected_paths();
        let has_selection = !selected.is_empty();
        if ui
            .add_enabled(has_selection, egui::Button::new("Open in Editor"))
            .clicked()
        {
            if let Some(path) = selected.first().cloned() {
                self.open_or_activate_tab(&path);
            }
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Reveal in Folder"))
            .clicked()
        {
            if let Some(path) = selected.first() {
                if let Err(err) = crate::app::helpers::open_folder_with_file_selected(path) {
                    self.push_toast(
                        crate::app::types::ToastSeverity::Warning,
                        format!("Reveal in folder failed: {err}"),
                    );
                }
            }
            ui.close();
        }
        // The Comments column opens the same conversation in place; this is
        // the way to it with that column hidden.
        if ui
            .add_enabled(has_selection, egui::Button::new("Comments..."))
            .clicked()
        {
            if let Some(path) = selected.first().cloned() {
                self.open_comments_window_for_path(&path);
            }
            ui.close();
        }
        ui.separator();
        if ui
            .add_enabled(has_selection, egui::Button::new("Copy to Clipboard"))
            .clicked()
        {
            self.copy_selected_to_clipboard();
            ui.close();
        }
        let can_paste = self.can_paste_into_list();
        if ui
            .add_enabled(can_paste, egui::Button::new("Paste"))
            .clicked()
        {
            self.paste_clipboard_to_list(None);
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Export Selected..."))
            .clicked()
        {
            self.trigger_save_selected();
            ui.close();
        }
        ui.separator();
        // Bulk assignment: the whole selection at once, which the per-row
        // dropdown in the Status/Tags cell also does but is easy to miss.
        let label_targets = selected.clone();
        let status_defs = self.status_palette.defs.clone();
        let current_status = self.shared_status_for_paths(&label_targets);
        let mut status_choice: Option<Option<String>> = None;
        ui.add_enabled_ui(has_selection, |ui| {
            ui.menu_button("Status", |ui| {
                if ui
                    .selectable_label(has_selection && current_status.is_none(), "— (none)")
                    .clicked()
                {
                    status_choice = Some(None);
                    ui.close();
                }
                if status_defs.is_empty() {
                    ui.label(egui::RichText::new("No statuses defined yet").weak());
                }
                for def in &status_defs {
                    let selected = current_status.as_deref() == Some(&*def.id);
                    if Self::label_menu_entry(ui, def, selected).clicked() {
                        status_choice = Some(Some(def.id.to_string()));
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button("Edit Statuses...").clicked() {
                    self.open_status_tags_window(false);
                    ui.close();
                }
            });
        });
        if let Some(choice) = status_choice {
            self.set_status_for_paths(&label_targets, choice.as_deref());
        }

        let tag_defs = self.tag_palette.defs.clone();
        // "On" means every selected row already has it, so clicking removes it
        // from all of them; a mixed selection adds it to the rest.
        let tag_state: Vec<bool> = tag_defs
            .iter()
            .map(|def| {
                !label_targets.is_empty()
                    && label_targets.iter().all(|path| {
                        self.item_for_path(path)
                            .is_some_and(|item| item.has_tag(&def.id))
                    })
            })
            .collect();
        let mut tag_toggle: Option<(String, bool)> = None;
        ui.add_enabled_ui(has_selection, |ui| {
            ui.menu_button("Tags", |ui| {
                if tag_defs.is_empty() {
                    ui.label(egui::RichText::new("No tags defined yet").weak());
                }
                for (def, on) in tag_defs.iter().zip(&tag_state) {
                    if Self::label_menu_entry(ui, def, *on).clicked() {
                        tag_toggle = Some((def.id.to_string(), !*on));
                    }
                }
                ui.separator();
                if ui.button("Edit Tags...").clicked() {
                    self.open_status_tags_window(true);
                    ui.close();
                }
            });
        });
        if let Some((id, on)) = tag_toggle {
            self.set_tag_for_paths(&label_targets, &id, on);
        }
        ui.separator();

        let effect_targets = selected.clone();
        ui.menu_button("Effect", |ui| {
            let entries = self.effect_graph.library.entries.clone();
            if entries.is_empty() {
                ui.label("No templates");
            }
            for entry in entries {
                let resp = ui.add_enabled(entry.valid, egui::Button::new(entry.name.clone()));
                if resp.clicked() {
                    if let Err(err) = self
                        .apply_effect_graph_template_to_paths(&entry.template_id, &effect_targets)
                    {
                        self.push_effect_graph_console(
                            crate::app::types::EffectGraphSeverity::Error,
                            "apply",
                            err,
                            None,
                        );
                    }
                    ui.close();
                }
            }
        });
        ui.menu_button("Effect Graph", |ui| {
            let can_open = has_selection;
            if ui
                .add_enabled(can_open, egui::Button::new("Open"))
                .clicked()
            {
                if let Some(path) = selected.first().cloned() {
                    self.open_effect_graph_workspace();
                    self.effect_graph.tester.target_path = Some(path.clone());
                    self.effect_graph.tester.target_path_input = path.display().to_string();
                    self.effect_graph.tester.last_input_bus = None;
                    self.effect_graph.tester.last_input_audio = None;
                    self.effect_graph.tester.last_output_bus = None;
                    self.effect_graph.tester.last_output_audio = None;
                    self.effect_graph.tester.playback_target = None;
                }
                ui.close();
            }
        });
        // The menu re-runs this every frame it is open, over the whole
        // selection. Statting here would be one blocking syscall per
        // selected file per frame — on a share, a hung menu. Resolve
        // existence through the background service instead.
        let mut transcript_targets: Vec<std::path::PathBuf> = Vec::new();
        for path in selected.iter() {
            let is_file_source = self
                .item_for_path(path)
                .map(|item| item.source == crate::app::types::MediaSource::File)
                .unwrap_or(false);
            if is_file_source
                && crate::audio_io::is_supported_audio_path(path)
                && self.path_is_file_cached(path)
            {
                transcript_targets.push(path.clone());
            }
        }
        let transcript_running = self.transcript_ai_is_running();
        let transcript_ready = self.transcript_ai_menu_enabled();
        let has_transcript_targets = !transcript_targets.is_empty();
        let transcript_enabled = transcript_running || (transcript_ready && has_transcript_targets);
        let transcript_label = if transcript_running {
            "Transcript (AI) - Cancel"
        } else {
            "Transcript (AI)"
        };
        let transcript_resp =
            ui.add_enabled(transcript_enabled, egui::Button::new(transcript_label));
        if transcript_resp.clicked() {
            if transcript_running {
                self.cancel_transcript_ai_run();
            } else {
                self.run_transcript_ai_for_selected(transcript_targets);
            }
            ui.close();
        }
        if !transcript_enabled {
            let reason = if !has_transcript_targets {
                "Select at least one real audio file.".to_string()
            } else {
                self.transcript_ai_unavailable_reason()
                    .unwrap_or_else(|| "Transcript AI is unavailable.".to_string())
            };
            transcript_resp.on_hover_text(reason);
        }
        let renameable_selected = self.selected_renameable_paths();
        if renameable_selected.len() == 1 {
            if ui.button("Rename (F2, inline)").clicked() {
                self.begin_inline_rename(renameable_selected[0].clone());
                ui.close();
            }
            if ui.button("Rename...").clicked() {
                self.open_rename_dialog(renameable_selected[0].clone());
                ui.close();
            }
        }
        let mut can_convert_bits = !selected.is_empty();
        for p in selected.iter() {
            if !can_convert_bits {
                break;
            }
            let is_wav = p
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("wav"))
                .unwrap_or(false);
            let is_file_source = self
                .item_for_path(p)
                .map(|item| item.source == crate::app::types::MediaSource::File)
                .unwrap_or(false);
            // Same reasoning as the transcript targets above: no stat here.
            can_convert_bits = is_wav
                && is_file_source
                && crate::media_kind::source_allows_export(p)
                && self.path_is_file_cached(p);
        }
        let convert_targets = if can_convert_bits {
            selected.clone()
        } else {
            Vec::new()
        };
        ui.menu_button("Convert Bits", |ui| {
            if ui
                .add_enabled(can_convert_bits, egui::Button::new("16-bit PCM"))
                .clicked()
            {
                self.spawn_convert_bits_selected(
                    convert_targets.clone(),
                    crate::wave::WavBitDepth::Pcm16,
                );
                ui.close();
            }
            if ui
                .add_enabled(can_convert_bits, egui::Button::new("24-bit PCM"))
                .clicked()
            {
                self.spawn_convert_bits_selected(
                    convert_targets.clone(),
                    crate::wave::WavBitDepth::Pcm24,
                );
                ui.close();
            }
            if ui
                .add_enabled(can_convert_bits, egui::Button::new("32-bit float"))
                .clicked()
            {
                self.spawn_convert_bits_selected(
                    convert_targets.clone(),
                    crate::wave::WavBitDepth::Float32,
                );
                ui.close();
            }
        });
        // A video source has no encodable form — the app can read its audio but
        // has no video encoder to write one back — so converting it is refused
        // rather than silently producing an audio-only file with a .mp4 name.
        let can_convert_format = has_selection
            && selected
                .iter()
                .all(|p| crate::media_kind::source_allows_export(p));
        let convert_format_disabled_reason =
            "Video sources are read-only in this version — their audio can be played and previewed, but not written back out.";
        ui.menu_button("Convert Format", |ui| {
            // MP3 and AAC encoding are optional features, so a build can be
            // missing them; offering a format the encoder cannot write would
            // only fail at the end of the export.
            for (label, ext) in [
                ("To WAV", "wav"),
                ("To AIFF", "aiff"),
                ("To FLAC", "flac"),
                ("To MP3", "mp3"),
                ("To M4A", "m4a"),
                ("To OGG", "ogg"),
            ] {
                let missing_encoder = crate::wave::export_format_unavailable_reason(ext);
                let enabled = can_convert_format && missing_encoder.is_none();
                let button = ui.add_enabled(enabled, egui::Button::new(label));
                let button = match (enabled, missing_encoder) {
                    (true, _) => button,
                    (false, Some(reason)) => button.on_disabled_hover_text(reason),
                    (false, None) => button.on_disabled_hover_text(convert_format_disabled_reason),
                };
                if button.clicked() {
                    self.spawn_convert_format_selected(selected.clone(), ext);
                    ui.close();
                }
            }
        });
        if ui
            .add_enabled(has_selection, egui::Button::new("Remove from List"))
            .clicked()
        {
            self.remove_paths_from_list_with_undo(&selected);
            ui.close();
        }
        let has_edits = self.has_edits_for_paths(&selected);
        if ui
            .add_enabled(has_edits, egui::Button::new("Clear Edits"))
            .clicked()
        {
            self.clear_edits_for_paths(&selected);
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Sample Rate Convert..."))
            .clicked()
        {
            self.open_resample_dialog(selected.clone());
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Inspect Selected (QA)..."))
            .clicked()
        {
            self.open_inspection_dialog();
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Normalize Loudness..."))
            .clicked()
        {
            self.open_loudnorm_dialog();
            ui.close();
        }
        ui.separator();
        if ui
            .add_enabled(!self.files.is_empty(), egui::Button::new("Select All"))
            .clicked()
        {
            self.list_select_all();
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Clear Selection"))
            .clicked()
        {
            self.list_clear_selection();
            ui.close();
        }
    }

    /// Right-click selection rule: clicking inside the current
    /// multi-selection keeps it; clicking outside selects that row.
    pub(crate) fn handle_row_secondary_click(&mut self, row_idx: usize, mods: egui::Modifiers) {
        if !self.selected_multi.contains(&row_idx) {
            self.update_selection_on_click(row_idx, mods);
        }
    }

    pub(super) fn attach_row_context_menu(
        &mut self,
        resp: egui::Response,
        row_idx: usize,
        ctx: &egui::Context,
    ) -> egui::Response {
        if resp.secondary_clicked() {
            let mods = ctx.input(|i| i.modifiers);
            self.handle_row_secondary_click(row_idx, mods);
        }
        resp.context_menu(|ui| {
            self.list_row_context_menu_contents(ui);
        });
        resp
    }
}
