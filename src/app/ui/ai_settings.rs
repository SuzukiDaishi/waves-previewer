use crate::app::assistant_ops::CloudUploadPolicy;

#[cfg(feature = "gemini")]
use zeroize::Zeroize;

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_gemini_settings_window(&mut self, ctx: &egui::Context) {
        if !self.assistant_state.show_settings {
            return;
        }
        let mut open = self.assistant_state.show_settings;
        let mut save_prefs = false;
        #[cfg(feature = "gemini")]
        let mut refresh_status = false;
        #[cfg(not(feature = "gemini"))]
        let refresh_status = false;
        #[cfg(feature = "gemini")]
        let mut save_key = false;
        #[cfg(feature = "gemini")]
        let mut save_key_succeeded = false;
        #[cfg(feature = "gemini")]
        let mut delete_key = false;
        let mut pick_ucs_catalog = false;
        let mut clear_ucs_catalog = false;
        egui::Window::new("Gemini Assistant Settings")
            .open(&mut open)
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(true)
            .default_width(470.0)
            .show(ctx, |ui| {
                #[cfg(not(feature = "gemini"))]
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "This build does not include the `gemini` Cargo feature.",
                );
                #[cfg(feature = "gemini")]
                {
                    let status = if self.assistant_state.configured {
                        "API key: configured"
                    } else {
                        "API key: not configured"
                    };
                    ui.label(status);
                    if std::env::var_os("GEMINI_API_KEY").is_some() {
                        ui.small("GEMINI_API_KEY environment override is active.");
                    }
                    ui.horizontal(|ui| {
                        ui.label("API key");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.assistant_state.api_key_input)
                                .password(true)
                                .hint_text("Stored in the OS credential manager"),
                        );
                    });
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !self.assistant_state.api_key_input.trim().is_empty(),
                                egui::Button::new("Save to OS credential store"),
                            )
                            .clicked()
                        {
                            save_key = true;
                        }
                        if ui.button("Remove stored key").clicked() {
                            delete_key = true;
                        }
                        if ui.button("Refresh status").clicked() {
                            refresh_status = true;
                        }
                    });
                    if let Some(status) = &self.assistant_state.settings_status {
                        ui.label(status);
                    }
                }

                ui.separator();
                if ui
                    .checkbox(
                        &mut self.assistant_state.enabled,
                        "Enable Gemini integration",
                    )
                    .changed()
                {
                    save_prefs = true;
                }
                if ui
                    .checkbox(
                        &mut self.assistant_state.privacy_mode,
                        "Privacy mode (store=false)",
                    )
                    .on_hover_text(
                        "Disables server-side conversation history and Agent Actions. \
                         Each chat request is stateless.",
                    )
                    .changed()
                {
                    save_prefs = true;
                    #[cfg(feature = "gemini")]
                    {
                        self.assistant_state.agent = crate::ai::agent::AgentSession::default();
                    }
                }
                ui.label("Cloud audio upload policy");
                egui::ComboBox::from_id_salt("gemini_upload_policy")
                    .selected_text(self.assistant_state.upload_policy.prefs_name())
                    .show_ui(ui, |ui| {
                        save_prefs |= ui
                            .selectable_value(
                                &mut self.assistant_state.upload_policy,
                                CloudUploadPolicy::Never,
                                "Never",
                            )
                            .changed();
                        save_prefs |= ui
                            .selectable_value(
                                &mut self.assistant_state.upload_policy,
                                CloudUploadPolicy::Ask,
                                "Ask before each upload",
                            )
                            .changed();
                        save_prefs |= ui
                            .selectable_value(
                                &mut self.assistant_state.upload_policy,
                                CloudUploadPolicy::ThisSession,
                                "Allow for this session",
                            )
                            .changed();
                    });
                ui.small(
                    "Chat sends only the visible MediaId context. Audio is never uploaded \
                     automatically after import.",
                );

                ui.separator();
                ui.label("Models");
                ui.horizontal(|ui| {
                    ui.label("Endpoint");
                    save_prefs |= ui
                        .text_edit_singleline(&mut self.assistant_state.endpoint)
                        .changed();
                });
                ui.horizontal(|ui| {
                    ui.label("Interaction");
                    save_prefs |= ui
                        .text_edit_singleline(&mut self.assistant_state.interaction_model)
                        .changed();
                });
                ui.horizontal(|ui| {
                    ui.label("Embedding");
                    save_prefs |= ui
                        .text_edit_singleline(&mut self.assistant_state.embedding_model)
                        .changed();
                });
                ui.horizontal(|ui| {
                    ui.label("Dimensions");
                    save_prefs |= ui
                        .add(
                            egui::DragValue::new(&mut self.assistant_state.embedding_dimensions)
                                .range(128..=3072),
                        )
                        .changed();
                });
                ui.small(
                    "The endpoint must be an HTTPS API root ending in /v1 or /v1beta. Model \
                     identifiers and dimensions are included in cache identity. Audio embedding \
                     segmentation is required above 180 seconds.",
                );

                ui.separator();
                ui.label("UCS catalog");
                if let Some(path) = &self.assistant_state.ucs_catalog_path {
                    ui.monospace(path.to_string_lossy());
                } else {
                    ui.weak("No canonical UCS JSON catalog selected.");
                }
                ui.horizontal(|ui| {
                    if ui.button("Select UCS JSON...").clicked() {
                        pick_ucs_catalog = true;
                    }
                    if ui
                        .add_enabled(
                            self.assistant_state.ucs_catalog_path.is_some(),
                            egui::Button::new("Clear"),
                        )
                        .clicked()
                    {
                        clear_ucs_catalog = true;
                    }
                });
                ui.small(
                    "Accepted shape: a JSON array of category_id, subcategory_id, description \
                     entries, or an object with an entries array. Gemini may only return IDs \
                     present in this validated catalog.",
                );
            });

        #[cfg(feature = "gemini")]
        if save_key {
            let result = crate::ai::credentials::save_api_key(&self.assistant_state.api_key_input);
            self.assistant_state.api_key_input.zeroize();
            self.assistant_state.settings_status = Some(match result {
                Ok(()) => {
                    save_key_succeeded = true;
                    "API key saved to the OS credential store.".into()
                }
                Err(error) => error.to_string(),
            });
            refresh_status = true;
        }
        #[cfg(feature = "gemini")]
        if delete_key {
            self.assistant_state.settings_status =
                Some(match crate::ai::credentials::delete_stored_api_key() {
                    Ok(()) => "Stored API key removed.".into(),
                    Err(error) => error.to_string(),
                });
            refresh_status = true;
        }
        if refresh_status {
            self.refresh_gemini_credential_status();
            #[cfg(feature = "gemini")]
            {
                if save_key_succeeded {
                    self.assistant_state.settings_status = Some(
                        if self.assistant_state.configured {
                            "API key saved and verified from the OS credential store."
                        } else {
                            "The credential store reported success, but the API key could not be read back."
                        }
                        .into(),
                    );
                } else if !save_key && !delete_key {
                    #[cfg(feature = "kittest")]
                    {
                        self.assistant_state.settings_status =
                            Some("Refresh complete: Gemini API key is not configured.".into());
                    }
                    #[cfg(not(feature = "kittest"))]
                    {
                        self.assistant_state.settings_status =
                            Some(match crate::ai::credentials::load_api_key() {
                                Ok((_, crate::ai::credentials::CredentialSource::Environment)) => {
                                    "Refresh complete: configured from GEMINI_API_KEY.".into()
                                }
                                Ok((_, crate::ai::credentials::CredentialSource::Keyring)) => {
                                    "Refresh complete: configured from the OS credential store."
                                        .into()
                                }
                                Err(error) => format!("Refresh complete: {error}."),
                            });
                    }
                }
            }
        }
        if pick_ucs_catalog {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("UCS catalog", &["json"])
                .pick_file()
            {
                match crate::ai::models::load_ucs_catalog(&path) {
                    Ok(entries) => {
                        let count = entries.len();
                        self.assistant_state.ucs_catalog_path = Some(path);
                        self.assistant_state.ucs_catalog = Some(std::sync::Arc::new(entries));
                        self.assistant_state.settings_status =
                            Some(format!("Validated {count} UCS catalog entries."));
                        save_prefs = true;
                    }
                    Err(error) => {
                        self.assistant_state.settings_status = Some(error);
                    }
                }
            }
        }
        if clear_ucs_catalog {
            self.assistant_state.ucs_catalog_path = None;
            self.assistant_state.ucs_catalog = None;
            self.assistant_state.settings_status = Some("UCS catalog cleared.".into());
            save_prefs = true;
        }
        if save_prefs {
            #[cfg(feature = "gemini")]
            {
                self.assistant_runtime = None;
            }
            self.save_prefs();
            self.refresh_gemini_credential_status();
        }
        if !open {
            #[cfg(feature = "gemini")]
            self.assistant_state.api_key_input.zeroize();
        }
        self.assistant_state.show_settings = open;
    }
}
