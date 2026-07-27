use crate::ai::models::{AudioClassification, AudioKind, UcsAnalysis};

#[derive(Clone)]
struct ReviewRow {
    media_id: crate::app::types::MediaId,
    display_name: String,
    classification: Option<AudioClassification>,
    ucs: Option<UcsAnalysis>,
    transcript: Option<crate::ai::models::VoiceTranscript>,
    confirmed_class: Option<AudioKind>,
    has_confirmed_transcript: bool,
    manually_edited: bool,
}

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_ai_review_window(&mut self, ctx: &egui::Context) {
        if !self.assistant_state.show_review {
            return;
        }
        let rows: Vec<_> = self
            .items
            .iter()
            .filter_map(|item| {
                let metadata = item.ai_metadata.as_deref()?;
                (metadata.classification_suggestion.is_some()
                    || metadata.ucs_suggestion.is_some()
                    || metadata.transcript_suggestion.is_some()
                    || metadata.confirmed_class.is_some()
                    || metadata.confirmed_transcript.is_some())
                .then(|| ReviewRow {
                    media_id: item.id,
                    display_name: item.display_name.clone(),
                    classification: metadata.classification_suggestion.clone(),
                    ucs: metadata.ucs_suggestion.clone(),
                    transcript: metadata.transcript_suggestion.clone(),
                    confirmed_class: metadata.confirmed_class,
                    has_confirmed_transcript: metadata.confirmed_transcript.is_some(),
                    manually_edited: metadata.manually_edited,
                })
            })
            .collect();
        let pending_batch = self.assistant_state.pending_cloud_batch.clone();
        let embedding_batch = self.assistant_state.embedding_batch.clone();
        let visible_audio_count = self.gemini_visible_audio_ids().len();
        #[cfg(feature = "gemini")]
        let pending_agent_action = self.assistant_state.pending_agent_approval.clone();
        let pending_names: Vec<_> = pending_batch
            .as_ref()
            .map(|batch| {
                batch
                    .media_ids
                    .iter()
                    .filter_map(|id| self.item_for_id(*id))
                    .take(8)
                    .map(|item| item.display_name.clone())
                    .collect()
            })
            .unwrap_or_default();
        let similarity_rows: Vec<_> = self
            .assistant_state
            .similarity_results
            .iter()
            .filter_map(|result| {
                self.item_for_id(result.media_id)
                    .map(|item| (result.clone(), item.display_name.clone()))
            })
            .collect();
        let similarity_reference_name = self
            .assistant_state
            .similarity_reference_media_id
            .and_then(|media_id| self.item_for_id(media_id))
            .map(|item| item.display_name.clone());

        let mut open = self.assistant_state.show_review;
        let mut search_similarity = false;
        let mut select_similarity = false;
        let mut index_visible_embeddings = false;
        let mut cancel_embedding_batch = false;
        let mut approve_cloud = false;
        let mut reject_cloud = false;
        #[cfg(feature = "gemini")]
        let mut approve_agent_action = false;
        #[cfg(feature = "gemini")]
        let mut reject_agent_action = false;
        let mut accept_classification = Vec::new();
        let mut reject_classification = Vec::new();
        let mut manual_class = Vec::new();
        let mut accept_ucs = Vec::new();
        let mut reject_ucs = Vec::new();
        let mut accept_transcript = Vec::new();
        let mut reject_transcript = Vec::new();
        egui::Window::new("AI Review Queue")
            .open(&mut open)
            .order(egui::Order::Foreground)
            .default_width(680.0)
            .default_height(520.0)
            .show(ctx, |ui| {
                ui.heading("Suggestions and approvals");
                ui.label(
                    "AI suggestions remain separate from confirmed metadata. Re-analysis \
                     does not overwrite a confirmed or manually edited value.",
                );
                ui.separator();
                ui.group(|ui| {
                    ui.strong("Similarity search");
                    if let Some(reference_name) = &similarity_reference_name {
                        ui.label(format!("Reference sound: {reference_name}"));
                    }
                    ui.horizontal(|ui| {
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut self.assistant_state.similarity_query)
                                .hint_text("e.g. heavy metal door closing indoors"),
                        );
                        if ui
                            .add_enabled(
                                !self.assistant_state.similarity_query.trim().is_empty(),
                                egui::Button::new("Search index"),
                            )
                            .clicked()
                            || (response.has_focus()
                                && ui.input(|input| input.key_pressed(egui::Key::Enter)))
                        {
                            search_similarity = true;
                        }
                    });
                    let embedding_batch_running = embedding_batch
                        .as_ref()
                        .is_some_and(|batch| !batch.job_ids.is_empty());
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                visible_audio_count > 0 && !embedding_batch_running,
                                egui::Button::new(format!(
                                    "Index visible List ({visible_audio_count})"
                                )),
                            )
                            .clicked()
                        {
                            index_visible_embeddings = true;
                        }
                        if embedding_batch_running
                            && ui.button("Cancel embedding batch").clicked()
                        {
                            cancel_embedding_batch = true;
                        }
                    });
                    if let Some(batch) = &embedding_batch {
                        ui.small(format!(
                            "Embedding: {}/{} · unchanged {} · new/changed {} · failed {}",
                            batch.completed,
                            batch.total,
                            batch.cached,
                            batch.embedded,
                            batch.failed
                        ));
                    }
                    if !similarity_rows.is_empty() {
                        for (result, name) in similarity_rows.iter().take(12) {
                            ui.label(format!(
                                "{:.3} · {} · {:.2}–{:.2}s",
                                result.score,
                                name,
                                result.segment_start_ms as f64 / 1000.0,
                                result.segment_end_ms as f64 / 1000.0
                            ));
                        }
                        if ui.button("Select results in List").clicked() {
                            select_similarity = true;
                        }
                    } else {
                        ui.small("Index selected WAV/MP3 files before searching.");
                    }
                });

                if let Some(batch) = &pending_batch {
                    ui.separator();
                    ui.group(|ui| {
                        ui.colored_label(
                            egui::Color32::from_rgb(251, 191, 36),
                            "Cloud audio upload approval required",
                        );
                        ui.strong(format!(
                            "{} · {} file(s)",
                            batch.kind.label(),
                            batch.media_ids.len()
                        ));
                        ui.label(format!(
                            "Estimated sent audio: {} s · about {} audio tokens",
                            batch.estimated_audio_seconds, batch.estimated_audio_tokens
                        ));
                        if batch.unknown_duration_count > 0 {
                            ui.weak(format!(
                                "{} file(s) have unknown duration and are not included in the estimate.",
                                batch.unknown_duration_count
                            ));
                        }
                        if let Some(count) = batch.ucs_candidate_count {
                            ui.label(format!(
                                "Trusted UCS candidate set: {count} validated entries"
                            ));
                        }
                        for name in &pending_names {
                            ui.label(format!("• {name}"));
                        }
                        if batch.media_ids.len() > pending_names.len() {
                            ui.weak(format!(
                                "… and {} more",
                                batch.media_ids.len() - pending_names.len()
                            ));
                        }
                        ui.small(
                            "Audio bytes will be sent to Gemini. Import never enables this \
                             automatically. Larger analysis files use the temporary Files API; \
                             embeddings are split into bounded segments. The estimate uses 32 \
                             audio tokens/second and may differ from actual usage.",
                        );
                        ui.horizontal(|ui| {
                            if ui.button("Approve this batch").clicked() {
                                approve_cloud = true;
                            }
                            if ui.button("Cancel").clicked() {
                                reject_cloud = true;
                            }
                        });
                    });
                }

                ui.separator();
                #[cfg(feature = "gemini")]
                if let Some(pending) = &pending_agent_action {
                    ui.group(|ui| {
                        ui.colored_label(
                            egui::Color32::from_rgb(251, 191, 36),
                            format!("Agent Action approval · {:?}", pending.approval.risk),
                        );
                        ui.strong(&pending.approval.summary);
                        ui.label(format!("Action: {}", pending.approval.action));
                        ui.small(format!(
                            "Context revision: {} · Approval expires automatically",
                            pending.approval.context_revision
                        ));
                        egui::CollapsingHeader::new("Execution plan")
                            .default_open(false)
                            .show(ui, |ui| {
                                ui.monospace(
                                    serde_json::to_string_pretty(&pending.approval.plan)
                                        .unwrap_or_else(|_| "{}".into()),
                                );
                            });
                        ui.label(
                            "The token is single-use and bound to these arguments, this plan, \
                             and the current context revision.",
                        );
                        ui.horizontal(|ui| {
                            if ui.button("Approve Action").clicked() {
                                approve_agent_action = true;
                            }
                            if ui.button("Reject Action").clicked() {
                                reject_agent_action = true;
                            }
                        });
                    });
                    ui.separator();
                }

                if rows.is_empty() {
                    ui.weak("No classification or UCS suggestions are waiting.");
                } else {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for row in &rows {
                            ui.group(|ui| {
                                ui.horizontal(|ui| {
                                    ui.strong(&row.display_name);
                                    ui.monospace(format!("MediaId {}", row.media_id));
                                });
                                if let Some(confirmed) = row.confirmed_class {
                                    ui.label(format!(
                                        "Confirmed: {confirmed:?}{}",
                                        if row.manually_edited {
                                            " · manual"
                                        } else {
                                            ""
                                        }
                                    ));
                                }
                                if row.has_confirmed_transcript {
                                    ui.label("Transcript: confirmed");
                                }
                                if let Some(suggestion) = &row.classification {
                                    let confidence = suggestion.confidence * 100.0;
                                    let color = if suggestion.confidence < 0.8 {
                                        egui::Color32::from_rgb(251, 191, 36)
                                    } else {
                                        egui::Color32::from_rgb(125, 211, 252)
                                    };
                                    ui.colored_label(
                                        color,
                                        format!(
                                            "Suggested: {:?} · {confidence:.0}%",
                                            suggestion.primary_class
                                        ),
                                    );
                                    if !suggestion.description_ja.is_empty() {
                                        ui.label(&suggestion.description_ja);
                                    }
                                    ui.horizontal_wrapped(|ui| {
                                        if ui.button("Accept suggestion").clicked() {
                                            accept_classification.push(row.media_id);
                                        }
                                        for (label, class) in [
                                            ("SE", AudioKind::Se),
                                            ("Voice", AudioKind::Voice),
                                            ("Music", AudioKind::Music),
                                            ("Other", AudioKind::Other),
                                        ] {
                                            if ui.small_button(label).clicked() {
                                                manual_class.push((row.media_id, class));
                                            }
                                        }
                                        if ui.button("Reject").clicked() {
                                            reject_classification.push(row.media_id);
                                        }
                                    });
                                }
                                if let Some(ucs) = &row.ucs {
                                    if let Some(best) = ucs.candidates.first() {
                                        ui.label(format!(
                                            "UCS: {} / {} · {:.0}%",
                                            best.category_id,
                                            best.subcategory_id,
                                            best.confidence * 100.0
                                        ));
                                        ui.horizontal(|ui| {
                                            if ui.button("Accept UCS").clicked() {
                                                accept_ucs.push(row.media_id);
                                            }
                                            if ui.button("Reject UCS").clicked() {
                                                reject_ucs.push(row.media_id);
                                            }
                                        });
                                    }
                                }
                                if let Some(transcript) = &row.transcript {
                                    ui.separator();
                                    ui.label(format!(
                                        "Transcript suggestion: {} · {} segment(s)",
                                        if transcript.language.is_empty() {
                                            "language unknown"
                                        } else {
                                            transcript.language.as_str()
                                        },
                                        transcript.segments.len()
                                    ));
                                    if !transcript.full_text.is_empty() {
                                        let preview: String =
                                            transcript.full_text.chars().take(240).collect();
                                        ui.label(preview);
                                    }
                                    ui.horizontal(|ui| {
                                        if ui.button("Accept transcript").clicked() {
                                            accept_transcript.push(row.media_id);
                                        }
                                        if ui.button("Reject transcript").clicked() {
                                            reject_transcript.push(row.media_id);
                                        }
                                    });
                                }
                            });
                            ui.add_space(6.0);
                        }
                    });
                }
            });

        if approve_cloud {
            self.approve_pending_gemini_cloud_batch();
        }
        if reject_cloud {
            self.reject_pending_gemini_cloud_batch();
        }
        #[cfg(feature = "gemini")]
        if approve_agent_action {
            self.approve_pending_gemini_action();
        }
        #[cfg(feature = "gemini")]
        if reject_agent_action {
            self.reject_pending_gemini_action();
        }
        if search_similarity {
            self.start_gemini_similarity_search();
        }
        if index_visible_embeddings {
            self.request_gemini_visible_embedding_index();
        }
        if cancel_embedding_batch {
            self.cancel_gemini_embedding_batch();
        }
        if select_similarity {
            self.select_gemini_similarity_results_in_list();
        }
        let mut confirmed_changed = false;
        for media_id in accept_classification {
            if let Some(item) = self.item_for_id_mut(media_id) {
                if let Some(metadata) = item.ai_metadata.as_deref_mut() {
                    if let Some(suggestion) = metadata.classification_suggestion.take() {
                        metadata.confirmed_class = Some(suggestion.primary_class);
                        metadata.manually_edited = false;
                        confirmed_changed = true;
                    }
                }
            }
        }
        for (media_id, class) in manual_class {
            if let Some(item) = self.item_for_id_mut(media_id) {
                let metadata = item.ai_metadata.get_or_insert_with(Default::default);
                metadata.confirmed_class = Some(class);
                metadata.classification_suggestion = None;
                metadata.manually_edited = true;
                confirmed_changed = true;
            }
        }
        for media_id in reject_classification {
            if let Some(item) = self.item_for_id_mut(media_id) {
                if let Some(metadata) = item.ai_metadata.as_deref_mut() {
                    metadata.classification_suggestion = None;
                }
            }
        }
        for media_id in accept_ucs {
            if let Some(item) = self.item_for_id_mut(media_id) {
                if let Some(metadata) = item.ai_metadata.as_deref_mut() {
                    if let Some(best) = metadata
                        .ucs_suggestion
                        .as_ref()
                        .and_then(|value| value.candidates.first())
                        .cloned()
                    {
                        metadata.confirmed_ucs_category_id = Some(best.category_id);
                        metadata.confirmed_ucs_subcategory_id = Some(best.subcategory_id);
                        metadata.confirmed_descriptors = best.descriptors;
                        metadata.ucs_suggestion = None;
                        confirmed_changed = true;
                    }
                }
            }
        }
        for media_id in reject_ucs {
            if let Some(item) = self.item_for_id_mut(media_id) {
                if let Some(metadata) = item.ai_metadata.as_deref_mut() {
                    metadata.ucs_suggestion = None;
                }
            }
        }
        for media_id in accept_transcript {
            if let Some(item) = self.item_for_id_mut(media_id) {
                let transcript = item
                    .ai_metadata
                    .as_deref_mut()
                    .and_then(|metadata| metadata.transcript_suggestion.take());
                if let Some(transcript) = transcript {
                    item.transcript = Some(std::sync::Arc::new(crate::app::types::Transcript {
                        segments: transcript
                            .segments
                            .iter()
                            .map(|segment| crate::app::types::TranscriptSegment {
                                start_ms: segment.start_ms,
                                end_ms: segment.end_ms,
                                text: segment.text.clone(),
                            })
                            .collect(),
                        full_text: transcript.full_text.clone(),
                    }));
                    if let Some(metadata) = item.ai_metadata.as_deref_mut() {
                        metadata.confirmed_transcript = Some(transcript);
                    }
                    confirmed_changed = true;
                }
            }
        }
        for media_id in reject_transcript {
            if let Some(item) = self.item_for_id_mut(media_id) {
                if let Some(metadata) = item.ai_metadata.as_deref_mut() {
                    metadata.transcript_suggestion = None;
                }
            }
        }
        if confirmed_changed {
            self.assistant_state.confirmed_metadata_dirty = true;
        }
        self.assistant_state.show_review = open;
    }
}
