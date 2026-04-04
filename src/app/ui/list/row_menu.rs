use crate::app::WavesPreviewer;

impl WavesPreviewer {
    fn list_row_context_menu_contents(&mut self, ui: &mut egui::Ui) {
        let selected = self.selected_paths();
        let has_selection = !selected.is_empty();
        if ui
            .add_enabled(has_selection, egui::Button::new("Copy to Clipboard"))
            .clicked()
        {
            self.copy_selected_to_clipboard();
            ui.close();
        }
        let can_paste = self
            .clipboard_payload
            .as_ref()
            .map(|p| !p.items.is_empty())
            .unwrap_or(false)
            || !self.get_clipboard_files().is_empty();
        if ui
            .add_enabled(can_paste, egui::Button::new("Paste"))
            .clicked()
        {
            self.paste_clipboard_to_list();
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Export Selected..."))
            .clicked()
        {
            self.trigger_save_selected();
            ui.close();
        }
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
        let transcript_targets: Vec<_> = selected
            .iter()
            .filter(|path| {
                self.item_for_path(path)
                    .map(|item| item.source == crate::app::types::MediaSource::File)
                    .unwrap_or(false)
                    && path.is_file()
                    && crate::audio_io::is_supported_audio_path(path)
            })
            .cloned()
            .collect();
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
            if ui.button("Rename...").clicked() {
                self.open_rename_dialog(renameable_selected[0].clone());
                ui.close();
            }
        }
        let can_convert_bits = !selected.is_empty()
            && selected.iter().all(|p| {
                let is_wav = p
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("wav"))
                    .unwrap_or(false);
                is_wav
                    && p.is_file()
                    && self
                        .item_for_path(p)
                        .map(|item| item.source == crate::app::types::MediaSource::File)
                        .unwrap_or(false)
            });
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
        ui.menu_button("Convert Format", |ui| {
            if ui
                .add_enabled(has_selection, egui::Button::new("To WAV"))
                .clicked()
            {
                self.spawn_convert_format_selected(selected.clone(), "wav");
                ui.close();
            }
            if ui
                .add_enabled(has_selection, egui::Button::new("To MP3"))
                .clicked()
            {
                self.spawn_convert_format_selected(selected.clone(), "mp3");
                ui.close();
            }
            if ui
                .add_enabled(has_selection, egui::Button::new("To M4A"))
                .clicked()
            {
                self.spawn_convert_format_selected(selected.clone(), "m4a");
                ui.close();
            }
            if ui
                .add_enabled(has_selection, egui::Button::new("To OGG"))
                .clicked()
            {
                self.spawn_convert_format_selected(selected.clone(), "ogg");
                ui.close();
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
    }

    pub(super) fn attach_row_context_menu(
        &mut self,
        resp: egui::Response,
        row_idx: usize,
        ctx: &egui::Context,
    ) -> egui::Response {
        if resp.secondary_clicked() && !self.selected_multi.contains(&row_idx) {
            let mods = ctx.input(|i| i.modifiers);
            self.update_selection_on_click(row_idx, mods);
        }
        resp.context_menu(|ui| {
            self.list_row_context_menu_contents(ui);
        });
        resp
    }
}
