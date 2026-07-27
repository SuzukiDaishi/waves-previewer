use crate::app::WavesPreviewer;

impl WavesPreviewer {
    fn list_row_context_menu_contents(&mut self, ui: &mut egui::Ui) {
        let selected = self.selected_paths();
        let has_selection = !selected.is_empty();
        let has_file_audio_targets = selected.iter().any(|path| {
            self.item_for_path(path)
                .is_some_and(|item| item.source == crate::app::types::MediaSource::File)
                && path.is_file()
                && crate::audio_io::is_supported_audio_path(path)
        });
        let can_find_similar = selected.len() == 1 && has_file_audio_targets;

        if ui
            .add_enabled(has_selection, egui::Button::new("Open in Editor"))
            .clicked()
        {
            if let Some(path) = selected.first().cloned() {
                self.open_or_activate_tab(&path);
            }
            ui.close();
        }

        let similar_response = ui.add_enabled(
            can_find_similar,
            egui::Button::new("Find Similar Sounds..."),
        );
        if similar_response.clicked() {
            self.request_gemini_direct_analysis(
                crate::app::assistant_ops::DirectAnalysisKind::FindSimilarAudio,
            );
            ui.close();
        }
        if !can_find_similar {
            similar_response.on_disabled_hover_text(
                "Select exactly one file-backed audio item to use as the reference sound.",
            );
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

        ui.separator();
        ui.menu_button("Clipboard", |ui| {
            if ui
                .add_enabled(has_selection, egui::Button::new("Copy Selected"))
                .clicked()
            {
                self.copy_selected_to_clipboard();
                ui.close();
            }
            let can_paste = self
                .clipboard_payload
                .as_ref()
                .is_some_and(|payload| !payload.items.is_empty())
                || !self.get_clipboard_files().is_empty();
            if ui
                .add_enabled(can_paste, egui::Button::new("Paste"))
                .clicked()
            {
                self.paste_clipboard_to_list();
                ui.close();
            }
        });

        let transcript_targets: Vec<_> = selected
            .iter()
            .filter(|path| {
                self.item_for_path(path)
                    .is_some_and(|item| item.source == crate::app::types::MediaSource::File)
                    && path.is_file()
                    && crate::audio_io::is_supported_audio_path(path)
            })
            .cloned()
            .collect();
        let transcript_running = self.transcript_ai_is_running();
        let transcript_ready = self.transcript_ai_menu_enabled();
        let has_transcript_targets = !transcript_targets.is_empty();
        ui.menu_button("AI & Analysis", |ui| {
            if ui
                .add_enabled(
                    has_file_audio_targets,
                    egui::Button::new("Classify Selected with Gemini..."),
                )
                .clicked()
            {
                self.request_gemini_direct_analysis(
                    crate::app::assistant_ops::DirectAnalysisKind::Classify,
                );
                ui.close();
            }
            if ui
                .add_enabled(
                    has_file_audio_targets,
                    egui::Button::new("Index Selected for AI Search..."),
                )
                .clicked()
            {
                self.request_gemini_direct_analysis(
                    crate::app::assistant_ops::DirectAnalysisKind::IndexEmbedding,
                );
                ui.close();
            }
            let transcript_enabled =
                transcript_running || (transcript_ready && has_transcript_targets);
            let transcript_label = if transcript_running {
                "Transcript Selected (AI) - Cancel"
            } else {
                "Transcript Selected (AI)"
            };
            let transcript_response =
                ui.add_enabled(transcript_enabled, egui::Button::new(transcript_label));
            if transcript_response.clicked() {
                if transcript_running {
                    self.cancel_transcript_ai_run();
                } else {
                    self.run_transcript_ai_for_selected(transcript_targets.clone());
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
                transcript_response.on_disabled_hover_text(reason);
            }
            ui.separator();
            if ui
                .add_enabled(has_selection, egui::Button::new("Inspect Selected (QA)..."))
                .clicked()
            {
                self.open_inspection_dialog();
                ui.close();
            }
        });

        let effect_targets = selected.clone();
        let can_convert_bits = !selected.is_empty()
            && selected.iter().all(|path| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("wav"))
                    && path.is_file()
                    && self
                        .item_for_path(path)
                        .is_some_and(|item| item.source == crate::app::types::MediaSource::File)
            });
        let convert_targets = can_convert_bits
            .then(|| selected.clone())
            .unwrap_or_default();
        let has_edits = self.has_edits_for_paths(&selected);
        ui.menu_button("Processing", |ui| {
            if ui
                .add_enabled(has_selection, egui::Button::new("Normalize Loudness..."))
                .clicked()
            {
                self.open_loudnorm_dialog();
                ui.close();
            }
            if ui
                .add_enabled(has_selection, egui::Button::new("Sample Rate Convert..."))
                .clicked()
            {
                self.open_resample_dialog(selected.clone());
                ui.close();
            }
            ui.separator();
            ui.menu_button("Apply Effect", |ui| {
                let entries = self.effect_graph.library.entries.clone();
                if entries.is_empty() {
                    ui.label("No templates");
                }
                for entry in entries {
                    let response =
                        ui.add_enabled(entry.valid, egui::Button::new(entry.name.clone()));
                    if response.clicked() {
                        if let Err(err) = self.apply_effect_graph_template_to_paths(
                            &entry.template_id,
                            &effect_targets,
                        ) {
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
                if ui
                    .add_enabled(has_selection, egui::Button::new("Open with Selection"))
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
            ui.menu_button("Convert Bit Depth", |ui| {
                for (label, depth) in [
                    ("16-bit PCM", crate::wave::WavBitDepth::Pcm16),
                    ("24-bit PCM", crate::wave::WavBitDepth::Pcm24),
                    ("32-bit float", crate::wave::WavBitDepth::Float32),
                ] {
                    if ui
                        .add_enabled(can_convert_bits, egui::Button::new(label))
                        .clicked()
                    {
                        self.spawn_convert_bits_selected(convert_targets.clone(), depth);
                        ui.close();
                    }
                }
            });
            ui.menu_button("Convert Format", |ui| {
                for (label, format) in [
                    ("To WAV", "wav"),
                    ("To AIFF", "aiff"),
                    ("To FLAC", "flac"),
                    ("To MP3", "mp3"),
                    ("To M4A", "m4a"),
                    ("To OGG", "ogg"),
                ] {
                    if ui
                        .add_enabled(has_selection, egui::Button::new(label))
                        .clicked()
                    {
                        self.spawn_convert_format_selected(selected.clone(), format);
                        ui.close();
                    }
                }
            });
            ui.separator();
            if ui
                .add_enabled(has_edits, egui::Button::new("Clear Edits"))
                .clicked()
            {
                self.clear_edits_for_paths(&selected);
                ui.close();
            }
        });

        let renameable_selected = self.selected_renameable_paths();
        ui.menu_button("File / List", |ui| {
            if ui
                .add_enabled(has_selection, egui::Button::new("Export Selected..."))
                .clicked()
            {
                self.trigger_save_selected();
                ui.close();
            }
            if renameable_selected.len() == 1 {
                if ui.button("Rename Inline (F2)").clicked() {
                    self.begin_inline_rename(renameable_selected[0].clone());
                    ui.close();
                }
                if ui.button("Rename...").clicked() {
                    self.open_rename_dialog(renameable_selected[0].clone());
                    ui.close();
                }
            }
            ui.separator();
            if ui
                .add_enabled(has_selection, egui::Button::new("Remove from List"))
                .clicked()
            {
                self.remove_paths_from_list_with_undo(&selected);
                ui.close();
            }
        });

        ui.menu_button("Selection", |ui| {
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
        });
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
