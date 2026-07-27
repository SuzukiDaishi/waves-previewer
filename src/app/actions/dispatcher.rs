use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::super::types::{MediaId, WorkspaceView};
use super::super::WavesPreviewer;
use super::registry::ActionRegistry;
use super::{ActionError, ActionRequest, ActionResult, ApprovalStore, DispatchOutcome};

pub fn dispatch_action(
    app: &mut WavesPreviewer,
    registry: &ActionRegistry,
    approvals: &mut ApprovalStore,
    request: ActionRequest,
) -> Result<DispatchOutcome, ActionError> {
    let descriptor = registry
        .get(&request.action)
        .ok_or_else(|| ActionError::UnknownAction(request.action.clone()))?;
    validate_arguments_contract(descriptor, &request.arguments)?;
    let context = app.action_context_snapshot();
    if let Some(expected) = request.expected_context_revision {
        if expected != context.revision {
            return Err(ActionError::StaleContext {
                expected,
                actual: context.revision,
            });
        }
    }
    if descriptor.risk.requires_approval() {
        preflight_risk_action(app, &request)?;
    }
    if descriptor.risk.requires_approval() {
        let targets = request
            .arguments
            .get("media_id")
            .and_then(Value::as_u64)
            .and_then(|media_id| {
                let item = app.item_for_id(media_id)?;
                let mut target = json!({
                        "media_id": media_id,
                        "display_name": item.display_name
                });
                if let Some(tab) = app.tabs.iter().find(|tab| tab.path == item.path) {
                    target["editor_revision"] = json!(tab.viewport_source_generation);
                    target["samples_len"] = json!(tab.samples_len);
                    target["sample_rate"] = json!(tab.buffer_sample_rate);
                }
                Some(target)
            })
            .into_iter()
            .collect::<Vec<_>>();
        let plan = json!({
            "action": request.action,
            "arguments": request.arguments,
            "targets": targets,
            "undo": descriptor.supports_undo,
        });
        if let Some(token) = request.approval.as_ref() {
            approvals.consume(token, &request, context.revision, &plan)?;
        } else {
            let approval = approvals.request(
                &request,
                descriptor.risk,
                context.revision,
                descriptor.description,
                plan,
            );
            return Ok(DispatchOutcome::NeedsApproval { request: approval });
        }
    }

    let result = match request.action.as_str() {
        "actions.coverage" => ActionResult {
            action: request.action,
            summary: "Returned the Action coverage manifest.".into(),
            data: super::coverage::manifest_value(registry),
            affected_media_ids: Vec::new(),
        },
        "context.get" => ActionResult {
            action: request.action,
            summary: "Captured current NeoWaves context.".into(),
            data: {
                #[cfg(feature = "gemini")]
                {
                    super::super::assistant_ops::filtered_context_value(
                        &context,
                        app.assistant_state.include_session_context,
                        app.assistant_state.include_selection_context,
                    )
                }
                #[cfg(not(feature = "gemini"))]
                {
                    serde_json::to_value(context)
                        .map_err(|error| ActionError::Execution(error.to_string()))?
                }
            },
            affected_media_ids: Vec::new(),
        },
        "jobs.list" => ActionResult {
            action: request.action,
            summary: "Listed active background jobs.".into(),
            data: json!({
                "scan_in_progress": app.scan_in_progress,
                "metadata_pending": app.meta_inflight.len(),
                "transcription_pending": app.transcript_inflight.len(),
                "music_analysis_pending": app.music_ai_inflight.len(),
                "processing": app.processing.is_some(),
                "exporting": app.export_state.is_some()
            }),
            affected_media_ids: Vec::new(),
        },
        "session.inspect" => ActionResult {
            action: request.action,
            summary: "Inspected current session.".into(),
            data: json!({
                "open": app.project_path.is_some(),
                "library_count": app.items.len(),
                "visible_count": app.files.len(),
                "editor_tabs": app.tabs.len(),
                "workspace": context.workspace
            }),
            affected_media_ids: Vec::new(),
        },
        "list.inspect_selection" => {
            let selected = app.selected_item_ids();
            let rows: Vec<_> = selected
                .iter()
                .filter_map(|id| app.item_for_id(*id))
                .map(|item| {
                    json!({
                        "media_id": item.id,
                        "display_name": item.display_name,
                        "source": format!("{:?}", item.source).to_ascii_lowercase(),
                        "has_metadata": item.meta.is_some(),
                        "has_transcript": item.transcript.is_some(),
                        "classification_suggestion": item.ai_metadata.as_deref()
                            .and_then(|metadata| metadata.classification_suggestion.as_ref())
                            .map(|suggestion| json!({
                                "class": format!("{:?}", suggestion.primary_class)
                                    .to_ascii_lowercase(),
                                "confidence": suggestion.confidence
                            })),
                        "confirmed_class": item.ai_metadata.as_deref()
                            .and_then(|metadata| metadata.confirmed_class)
                            .map(|class| format!("{class:?}").to_ascii_lowercase())
                    })
                })
                .collect();
            ActionResult {
                action: request.action,
                summary: format!("Inspected {} selected item(s).", rows.len()),
                data: json!({"items": rows}),
                affected_media_ids: selected,
            }
        }
        "list.query" => {
            let query = required_string(&request.arguments, "query")?;
            let query_lower = query.to_lowercase();
            let limit = request
                .arguments
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(50)
                .clamp(1, 200) as usize;
            let mut results = Vec::new();
            for id in &app.files {
                let Some(item) = app.item_for_id(*id) else {
                    continue;
                };
                if item.display_name.to_lowercase().contains(&query_lower)
                    || item.display_folder.to_lowercase().contains(&query_lower)
                {
                    results.push(json!({
                        "media_id": item.id,
                        "display_name": item.display_name,
                        "display_folder": item.display_folder.as_ref()
                    }));
                    if results.len() >= limit {
                        break;
                    }
                }
            }
            let ids = results
                .iter()
                .filter_map(|value| value.get("media_id").and_then(Value::as_u64))
                .collect();
            ActionResult {
                action: request.action,
                summary: format!("Found {} bounded list result(s).", results.len()),
                data: json!({"items": results, "truncated_at": limit}),
                affected_media_ids: ids,
            }
        }
        "list.select" => {
            let requested = required_media_ids(&request.arguments)?;
            let requested_set: BTreeSet<_> = requested.iter().copied().collect();
            let mut selected_rows = BTreeSet::new();
            for (row, id) in app.files.iter().enumerate() {
                if requested_set.contains(id) {
                    selected_rows.insert(row);
                }
            }
            if selected_rows.len() != requested_set.len() {
                return Err(ActionError::Precondition(
                    "one or more MediaId values are not visible".into(),
                ));
            }
            app.selected_multi = selected_rows;
            app.selected = app.selected_multi.iter().next().copied();
            app.select_anchor = app.selected;
            ActionResult {
                action: request.action,
                summary: format!("Selected {} item(s).", requested.len()),
                data: json!({"selected_media_ids": requested}),
                affected_media_ids: requested,
            }
        }
        "ui.controls.list" => ActionResult {
            action: request.action,
            summary: "Listed allowlisted UI controls and current state.".into(),
            data: ui_controls_value(app),
            affected_media_ids: Vec::new(),
        },
        "ui.text.set" => {
            let control_id = required_string(&request.arguments, "control_id")?;
            let text = required_string(&request.arguments, "text")?.to_string();
            match control_id {
                "list.search.query" => {
                    app.search_query = text.clone();
                    app.refresh_filter_then_sort();
                    app.search_dirty = false;
                    app.search_deadline = None;
                }
                "ai.similarity.query" => {
                    app.assistant_state.similarity_query = text.clone();
                    app.assistant_state.show_review = true;
                }
                _ => {
                    return Err(ActionError::InvalidArguments(format!(
                        "unsupported text control_id: {control_id}"
                    )));
                }
            }
            ActionResult {
                action: request.action,
                summary: format!("Set text input {control_id}."),
                data: json!({"control_id": control_id, "text": text}),
                affected_media_ids: Vec::new(),
            }
        }
        "ui.checkbox.set" => {
            let control_id = required_string(&request.arguments, "control_id")?;
            let checked = request
                .arguments
                .get("checked")
                .and_then(Value::as_bool)
                .ok_or_else(|| ActionError::InvalidArguments("checked must be a boolean".into()))?;
            match control_id {
                "list.search.regex" => {
                    app.search_use_regex = checked;
                    app.refresh_filter_then_sort();
                    app.search_dirty = false;
                    app.search_deadline = None;
                }
                "list.auto_play" => {
                    app.auto_play_list_nav = checked;
                    app.save_prefs();
                }
                _ => {
                    return Err(ActionError::InvalidArguments(format!(
                        "unsupported checkbox control_id: {control_id}"
                    )));
                }
            }
            ActionResult {
                action: request.action,
                summary: format!("Set checkbox {control_id} to {checked}."),
                data: json!({"control_id": control_id, "checked": checked}),
                affected_media_ids: Vec::new(),
            }
        }
        "ui.button.press" => {
            let control_id = required_string(&request.arguments, "control_id")?;
            let (summary, data, affected_media_ids) = press_ui_button(app, control_id)?;
            ActionResult {
                action: request.action,
                summary,
                data,
                affected_media_ids,
            }
        }
        "workspace.show" => {
            let workspace = required_string(&request.arguments, "workspace")?;
            match workspace {
                "list" => app.workspace_view = WorkspaceView::List,
                "editor" => {
                    if app.active_tab.is_none() {
                        return Err(ActionError::Precondition(
                            "no editor tab is available".into(),
                        ));
                    }
                    app.workspace_view = WorkspaceView::Editor;
                }
                "effect_graph" => {
                    if !app.effect_graph.workspace_open {
                        return Err(ActionError::Precondition(
                            "Effect Graph workspace has not been opened".into(),
                        ));
                    }
                    app.workspace_view = WorkspaceView::EffectGraph;
                }
                "recording" => {
                    app.recording_tab.tab_open = true;
                    app.workspace_view = WorkspaceView::Recording;
                }
                _ => {
                    return Err(ActionError::InvalidArguments(
                        "workspace must be list, editor, effect_graph, or recording".into(),
                    ));
                }
            }
            ActionResult {
                action: request.action,
                summary: format!("Showing {workspace} workspace."),
                data: json!({"workspace": workspace}),
                affected_media_ids: Vec::new(),
            }
        }
        "transport.play_pause" => {
            app.request_workspace_play_toggle();
            ActionResult {
                action: request.action,
                summary: "Toggled workspace playback.".into(),
                data: json!({"playing": app.audio.shared.playing.load(std::sync::atomic::Ordering::Relaxed)}),
                affected_media_ids: Vec::new(),
            }
        }
        "transport.stop" => {
            app.audio.stop();
            app.playback_sync_state_snapshot();
            ActionResult {
                action: request.action,
                summary: "Stopped playback.".into(),
                data: json!({"playing": false}),
                affected_media_ids: Vec::new(),
            }
        }
        "editor.inspect" => {
            let media_id = required_media_id(&request.arguments)?;
            let tab_idx = loaded_editor_tab(app, media_id)?;
            let tab = &app.tabs[tab_idx];
            let sample_rate = u64::from(tab.buffer_sample_rate.max(1));
            ActionResult {
                action: request.action,
                summary: format!("Inspected loaded editor MediaId {media_id}."),
                data: json!({
                    "media_id": media_id,
                    "display_name": tab.display_name,
                    "duration_ms": (tab.samples_len as u64).saturating_mul(1000) / sample_rate,
                    "sample_rate": tab.buffer_sample_rate,
                    "channels": tab.ch_samples.len(),
                    "dirty": tab.dirty,
                    "selection": tab.selection.map(|(start, end)| json!({
                        "start_ms": (start as u64).saturating_mul(1000) / sample_rate,
                        "end_ms": (end as u64).saturating_mul(1000) / sample_rate
                    })),
                    "loop": tab.loop_region.map(|(start, end)| json!({
                        "start_ms": (start as u64).saturating_mul(1000) / sample_rate,
                        "end_ms": (end as u64).saturating_mul(1000) / sample_rate
                    })),
                    "markers": tab.markers.iter().take(200).map(|marker| json!({
                        "position_ms": (marker.sample as u64).saturating_mul(1000) / sample_rate,
                        "label": marker.label
                    })).collect::<Vec<_>>(),
                    "editor_revision": tab.viewport_source_generation
                }),
                affected_media_ids: vec![media_id],
            }
        }
        "editor.gain.apply" => {
            let media_id = required_media_id(&request.arguments)?;
            let gain_db = request
                .arguments
                .get("gain_db")
                .and_then(Value::as_f64)
                .filter(|value| value.is_finite() && (-24.0..=24.0).contains(value))
                .ok_or_else(|| {
                    ActionError::InvalidArguments("gain_db must be within -24..=24".into())
                })? as f32;
            let tab_idx = loaded_editor_tab(app, media_id)?;
            let range = optional_editor_range(app, tab_idx, &request.arguments)?
                .or(app.tabs[tab_idx].selection)
                .unwrap_or((0, app.tabs[tab_idx].samples_len));
            validate_sample_range(&app.tabs[tab_idx], range)?;
            app.editor_apply_gain_range_opts(tab_idx, range, gain_db, false);
            ActionResult {
                action: request.action,
                summary: format!("Applied {gain_db:+.2} dB to MediaId {media_id} with Undo."),
                data: editor_mutation_result(&app.tabs[tab_idx], media_id, range),
                affected_media_ids: vec![media_id],
            }
        }
        "editor.trim.apply" => {
            let media_id = required_media_id(&request.arguments)?;
            let tab_idx = loaded_editor_tab(app, media_id)?;
            let range = required_editor_range(app, tab_idx, &request.arguments)?;
            validate_sample_range(&app.tabs[tab_idx], range)?;
            app.editor_apply_trim_range(tab_idx, range);
            let new_range = (0, app.tabs[tab_idx].samples_len);
            ActionResult {
                action: request.action,
                summary: format!("Trimmed MediaId {media_id} with Undo."),
                data: editor_mutation_result(&app.tabs[tab_idx], media_id, new_range),
                affected_media_ids: vec![media_id],
            }
        }
        "loop.set" => {
            let media_id = required_media_id(&request.arguments)?;
            let tab_idx = loaded_editor_tab(app, media_id)?;
            let range = required_editor_range(app, tab_idx, &request.arguments)?;
            validate_sample_range(&app.tabs[tab_idx], range)?;
            let undo = {
                let tab = &mut app.tabs[tab_idx];
                let undo = WavesPreviewer::capture_undo_state_labeled(tab, "Set Marker Loop");
                tab.loop_region = Some(range);
                tab.loop_mode = super::super::types::LoopMode::Marker;
                WavesPreviewer::update_loop_markers_dirty(tab);
                undo
            };
            app.push_editor_undo_state(tab_idx, undo, true);
            if app.active_tab == Some(tab_idx) {
                let tab = &app.tabs[tab_idx];
                app.apply_loop_mode_for_tab(tab);
            }
            ActionResult {
                action: request.action,
                summary: format!("Set playback loop for MediaId {media_id} with Undo."),
                data: editor_mutation_result(&app.tabs[tab_idx], media_id, range),
                affected_media_ids: vec![media_id],
            }
        }
        "markers.add" => {
            let media_id = required_media_id(&request.arguments)?;
            let tab_idx = loaded_editor_tab(app, media_id)?;
            let position_ms = required_bounded_u64(&request.arguments, "position_ms", 86_400_000)?;
            let sample_rate = u64::from(app.tabs[tab_idx].buffer_sample_rate.max(1));
            let sample = position_ms
                .saturating_mul(sample_rate)
                .checked_div(1000)
                .unwrap_or(0) as usize;
            if sample >= app.tabs[tab_idx].samples_len {
                return Err(ActionError::Precondition(
                    "marker position is outside the loaded editor audio".into(),
                ));
            }
            if app.tabs[tab_idx]
                .markers
                .iter()
                .any(|marker| marker.sample == sample)
            {
                return Err(ActionError::Precondition(
                    "a marker already exists at this position".into(),
                ));
            }
            let requested_label = optional_bounded_string(&request.arguments, "label", 128)?;
            let label = requested_label
                .unwrap_or_else(|| WavesPreviewer::next_marker_label(&app.tabs[tab_idx].markers));
            let undo = {
                let tab = &mut app.tabs[tab_idx];
                let undo = WavesPreviewer::capture_undo_state_labeled(tab, "Add Marker");
                let marker = crate::markers::MarkerEntry {
                    sample,
                    label: label.clone(),
                };
                let insert_at = tab
                    .markers
                    .binary_search_by_key(&sample, |marker| marker.sample)
                    .unwrap_or_else(|index| index);
                tab.markers.insert(insert_at, marker);
                WavesPreviewer::update_markers_dirty(tab);
                undo
            };
            app.push_editor_undo_state(tab_idx, undo, true);
            ActionResult {
                action: request.action,
                summary: format!("Added marker {label:?} to MediaId {media_id} with Undo."),
                data: json!({
                    "media_id": media_id,
                    "position_ms": position_ms,
                    "label": label,
                    "editor_revision": app.tabs[tab_idx].viewport_source_generation
                }),
                affected_media_ids: vec![media_id],
            }
        }
        "labels.accept" => {
            let media_id = required_media_id(&request.arguments)?;
            let requested_class = required_audio_kind(&request.arguments)?;
            let item = app
                .item_for_id_mut(media_id)
                .ok_or_else(|| ActionError::Precondition("MediaId no longer exists".into()))?;
            let metadata = item.ai_metadata.as_deref_mut().ok_or_else(|| {
                ActionError::Precondition("AI suggestion no longer exists".into())
            })?;
            let suggestion = metadata.classification_suggestion.as_ref().ok_or_else(|| {
                ActionError::Precondition("AI suggestion no longer exists".into())
            })?;
            if suggestion.primary_class != requested_class {
                return Err(ActionError::Precondition(
                    "current AI suggestion differs from the approved class".into(),
                ));
            }
            metadata.confirmed_class = Some(requested_class);
            metadata.classification_suggestion = None;
            metadata.manually_edited = false;
            app.assistant_state.confirmed_metadata_dirty = true;
            ActionResult {
                action: request.action,
                summary: format!("Confirmed {requested_class:?} for MediaId {media_id}."),
                data: json!({
                    "media_id": media_id,
                    "confirmed_class": format!("{requested_class:?}").to_ascii_lowercase()
                }),
                affected_media_ids: vec![media_id],
            }
        }
        _ => return Err(ActionError::UnknownAction(request.action)),
    };
    Ok(DispatchOutcome::Completed { result })
}

fn ui_controls_value(app: &WavesPreviewer) -> Value {
    let selected_audio_ids = selected_file_audio_ids(app);
    let visible_audio_count = app.gemini_visible_audio_ids().len();
    let embedding_batch_running = app
        .assistant_state
        .embedding_batch
        .as_ref()
        .is_some_and(|batch| !batch.job_ids.is_empty());
    json!({
        "text_inputs": [
            {
                "control_id": "list.search.query",
                "value": app.search_query,
                "enabled": true
            },
            {
                "control_id": "ai.similarity.query",
                "value": app.assistant_state.similarity_query,
                "enabled": true
            }
        ],
        "checkboxes": [
            {
                "control_id": "list.search.regex",
                "checked": app.search_use_regex,
                "enabled": true
            },
            {
                "control_id": "list.auto_play",
                "checked": app.auto_play_list_nav,
                "enabled": true
            }
        ],
        "buttons": [
            {"control_id": "assistant.open", "enabled": true},
            {"control_id": "ai.review.open", "enabled": true},
            {
                "control_id": "ai.index_visible_embeddings",
                "enabled": visible_audio_count > 0 && !embedding_batch_running,
                "target_count": visible_audio_count
            },
            {
                "control_id": "ai.index_selected_embeddings",
                "enabled": !selected_audio_ids.is_empty() && !embedding_batch_running,
                "target_count": selected_audio_ids.len()
            },
            {
                "control_id": "ai.cancel_embedding_batch",
                "enabled": embedding_batch_running
            },
            {
                "control_id": "ai.search_similarity_text",
                "enabled": !app.assistant_state.similarity_query.trim().is_empty()
            },
            {
                "control_id": "ai.find_similar_selected",
                "enabled": selected_audio_ids.len() == 1
            },
            {
                "control_id": "ai.select_similarity_results",
                "enabled": !app.assistant_state.similarity_results.is_empty(),
                "target_count": app.assistant_state.similarity_results.len()
            },
            {
                "control_id": "list.search.clear",
                "enabled": !app.search_query.is_empty()
            },
            {
                "control_id": "list.selection.select_all",
                "enabled": !app.files.is_empty(),
                "target_count": app.files.len()
            },
            {
                "control_id": "list.selection.clear",
                "enabled": !app.selected_item_ids().is_empty()
            }
        ]
    })
}

fn press_ui_button(
    app: &mut WavesPreviewer,
    control_id: &str,
) -> Result<(String, Value, Vec<MediaId>), ActionError> {
    match control_id {
        "assistant.open" => {
            app.open_gemini_assistant();
            Ok((
                "Opened Gemini Assistant.".into(),
                json!({"control_id": control_id}),
                Vec::new(),
            ))
        }
        "ai.review.open" => {
            app.assistant_state.show_review = true;
            Ok((
                "Opened AI Review Queue.".into(),
                json!({"control_id": control_id}),
                Vec::new(),
            ))
        }
        "ai.index_visible_embeddings" => {
            let media_ids = app.gemini_visible_audio_ids();
            if media_ids.is_empty() {
                return Err(ActionError::Precondition(
                    "the visible List has no file-backed audio".into(),
                ));
            }
            let target_count = media_ids.len();
            app.request_gemini_visible_embedding_index();
            Ok((
                format!("Requested embedding indexing for {target_count} visible audio item(s)."),
                json!({
                    "control_id": control_id,
                    "target_count": target_count,
                    "upload_policy": app.assistant_state.upload_policy.prefs_name()
                }),
                media_ids.into_iter().take(200).collect(),
            ))
        }
        "ai.index_selected_embeddings" => {
            let media_ids = selected_file_audio_ids(app);
            if media_ids.is_empty() {
                return Err(ActionError::Precondition(
                    "no file-backed audio is selected".into(),
                ));
            }
            let target_count = media_ids.len();
            app.request_gemini_direct_analysis(
                super::super::assistant_ops::DirectAnalysisKind::IndexEmbedding,
            );
            Ok((
                format!("Requested embedding indexing for {target_count} selected item(s)."),
                json!({
                    "control_id": control_id,
                    "target_count": target_count,
                    "upload_policy": app.assistant_state.upload_policy.prefs_name()
                }),
                media_ids.into_iter().take(200).collect(),
            ))
        }
        "ai.cancel_embedding_batch" => {
            if !app
                .assistant_state
                .embedding_batch
                .as_ref()
                .is_some_and(|batch| !batch.job_ids.is_empty())
            {
                return Err(ActionError::Precondition(
                    "no embedding batch is running".into(),
                ));
            }
            app.cancel_gemini_embedding_batch();
            Ok((
                "Requested cancellation of the embedding batch.".into(),
                json!({"control_id": control_id}),
                Vec::new(),
            ))
        }
        "ai.search_similarity_text" => {
            if app.assistant_state.similarity_query.trim().is_empty() {
                return Err(ActionError::Precondition(
                    "the similarity query text input is empty".into(),
                ));
            }
            app.start_gemini_similarity_search();
            Ok((
                "Pressed Search index for the current similarity query.".into(),
                json!({"control_id": control_id}),
                Vec::new(),
            ))
        }
        "ai.find_similar_selected" => {
            let media_ids = selected_file_audio_ids(app);
            if media_ids.len() != 1 {
                return Err(ActionError::Precondition(
                    "select exactly one file-backed reference sound".into(),
                ));
            }
            app.request_gemini_direct_analysis(
                super::super::assistant_ops::DirectAnalysisKind::FindSimilarAudio,
            );
            Ok((
                "Requested similarity search from the selected reference sound.".into(),
                json!({
                    "control_id": control_id,
                    "upload_policy": app.assistant_state.upload_policy.prefs_name()
                }),
                media_ids,
            ))
        }
        "ai.select_similarity_results" => {
            if app.assistant_state.similarity_results.is_empty() {
                return Err(ActionError::Precondition(
                    "no similarity results are available".into(),
                ));
            }
            let count = app.select_gemini_similarity_results_in_list();
            Ok((
                format!("Selected {count} similarity result(s) in the List."),
                json!({"control_id": control_id, "selected_count": count}),
                app.selected_item_ids().into_iter().take(200).collect(),
            ))
        }
        "list.search.clear" => {
            app.search_query.clear();
            app.refresh_filter_then_sort();
            app.search_dirty = false;
            app.search_deadline = None;
            Ok((
                "Cleared the List search text input.".into(),
                json!({"control_id": control_id, "visible_count": app.files.len()}),
                Vec::new(),
            ))
        }
        "list.selection.select_all" => {
            if app.files.is_empty() {
                return Err(ActionError::Precondition(
                    "the visible List is empty".into(),
                ));
            }
            app.list_select_all();
            let selected = app.selected_item_ids();
            Ok((
                format!("Selected all {} visible List item(s).", selected.len()),
                json!({"control_id": control_id, "selected_count": selected.len()}),
                selected.into_iter().take(200).collect(),
            ))
        }
        "list.selection.clear" => {
            let previous_count = app.selected_item_ids().len();
            app.list_clear_selection();
            Ok((
                format!("Cleared {previous_count} selected List item(s)."),
                json!({"control_id": control_id, "cleared_count": previous_count}),
                Vec::new(),
            ))
        }
        _ => Err(ActionError::InvalidArguments(format!(
            "unsupported button control_id: {control_id}"
        ))),
    }
}

fn selected_file_audio_ids(app: &WavesPreviewer) -> Vec<MediaId> {
    app.selected_item_ids()
        .into_iter()
        .filter(|media_id| {
            app.item_for_id(*media_id).is_some_and(|item| {
                item.source == super::super::types::MediaSource::File
                    && item.path.is_file()
                    && crate::audio_io::is_supported_audio_path(&item.path)
            })
        })
        .collect()
}

fn required_string<'a>(arguments: &'a Value, key: &str) -> Result<&'a str, ActionError> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.chars().count() <= 4096)
        .ok_or_else(|| ActionError::InvalidArguments(format!("{key} must be a non-empty string")))
}

fn validate_arguments_contract(
    descriptor: &super::registry::ActionDescriptor,
    arguments: &Value,
) -> Result<(), ActionError> {
    let object = arguments.as_object().ok_or_else(|| {
        ActionError::InvalidArguments("Action arguments must be a JSON object".into())
    })?;
    if serde_json::to_vec(arguments)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
        > 64 * 1024
    {
        return Err(ActionError::InvalidArguments(
            "Action arguments exceed the 64 KiB limit".into(),
        ));
    }
    let properties = descriptor
        .arguments_schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| ActionError::Execution("Action schema has no properties".into()))?;
    for key in object.keys() {
        if !properties.contains_key(key) {
            return Err(ActionError::InvalidArguments(format!(
                "undeclared Action argument: {key}"
            )));
        }
    }
    for required in descriptor
        .arguments_schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !object.contains_key(required) {
            return Err(ActionError::InvalidArguments(format!(
                "missing required Action argument: {required}"
            )));
        }
    }
    for (key, value) in object {
        let schema = &properties[key];
        let type_ok = match schema.get("type").and_then(Value::as_str) {
            Some("string") => value.is_string(),
            Some("integer") => value.as_i64().is_some() || value.as_u64().is_some(),
            Some("number") => value.as_f64().is_some_and(f64::is_finite),
            Some("array") => value.is_array(),
            Some("boolean") => value.is_boolean(),
            Some("object") => value.is_object(),
            Some(_) | None => true,
        };
        if !type_ok {
            return Err(ActionError::InvalidArguments(format!(
                "{key} does not match its declared type"
            )));
        }
        if let Some(max_length) = schema.get("maxLength").and_then(Value::as_u64) {
            if value
                .as_str()
                .is_some_and(|text| text.chars().count() as u64 > max_length)
            {
                return Err(ActionError::InvalidArguments(format!(
                    "{key} exceeds its declared maximum length"
                )));
            }
        }
        if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64) {
            if value.as_f64().is_some_and(|number| number < minimum) {
                return Err(ActionError::InvalidArguments(format!(
                    "{key} is below its declared minimum"
                )));
            }
        }
        if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64) {
            if value.as_f64().is_some_and(|number| number > maximum) {
                return Err(ActionError::InvalidArguments(format!(
                    "{key} exceeds its declared maximum"
                )));
            }
        }
        if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
            if !allowed.contains(value) {
                return Err(ActionError::InvalidArguments(format!(
                    "{key} is outside its declared enum"
                )));
            }
        }
        if let Some(values) = value.as_array() {
            if let Some(max_items) = schema.get("maxItems").and_then(Value::as_u64) {
                if values.len() as u64 > max_items {
                    return Err(ActionError::InvalidArguments(format!(
                        "{key} exceeds its declared item limit"
                    )));
                }
            }
            if let Some(item_schema) = schema.get("items") {
                for item in values {
                    if item_schema.get("type").and_then(Value::as_str) == Some("integer")
                        && item.as_u64().is_none()
                    {
                        return Err(ActionError::InvalidArguments(format!(
                            "{key} contains a non-integer item"
                        )));
                    }
                    if let Some(minimum) = item_schema.get("minimum").and_then(Value::as_u64) {
                        if item.as_u64().is_some_and(|number| number < minimum) {
                            return Err(ActionError::InvalidArguments(format!(
                                "{key} contains an item below its declared minimum"
                            )));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn required_media_id(arguments: &Value) -> Result<MediaId, ActionError> {
    arguments
        .get("media_id")
        .and_then(Value::as_u64)
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            ActionError::InvalidArguments("media_id must be a positive stable MediaId".into())
        })
}

fn required_audio_kind(arguments: &Value) -> Result<crate::ai::models::AudioKind, ActionError> {
    match required_string(arguments, "class")? {
        "se" => Ok(crate::ai::models::AudioKind::Se),
        "voice" => Ok(crate::ai::models::AudioKind::Voice),
        "music" => Ok(crate::ai::models::AudioKind::Music),
        "other" => Ok(crate::ai::models::AudioKind::Other),
        _ => Err(ActionError::InvalidArguments(
            "class must be se, voice, music, or other".into(),
        )),
    }
}

fn preflight_risk_action(app: &WavesPreviewer, request: &ActionRequest) -> Result<(), ActionError> {
    match request.action.as_str() {
        "editor.gain.apply" => {
            let media_id = required_media_id(&request.arguments)?;
            let tab_idx = loaded_editor_tab(app, media_id)?;
            let gain_db = request
                .arguments
                .get("gain_db")
                .and_then(Value::as_f64)
                .filter(|value| value.is_finite() && (-24.0..=24.0).contains(value))
                .ok_or_else(|| {
                    ActionError::InvalidArguments("gain_db must be within -24..=24".into())
                })?;
            let _ = gain_db;
            let range = optional_editor_range(app, tab_idx, &request.arguments)?
                .or(app.tabs[tab_idx].selection)
                .unwrap_or((0, app.tabs[tab_idx].samples_len));
            validate_sample_range(&app.tabs[tab_idx], range)
        }
        "editor.trim.apply" | "loop.set" => {
            let media_id = required_media_id(&request.arguments)?;
            let tab_idx = loaded_editor_tab(app, media_id)?;
            let range = required_editor_range(app, tab_idx, &request.arguments)?;
            validate_sample_range(&app.tabs[tab_idx], range)
        }
        "markers.add" => {
            let media_id = required_media_id(&request.arguments)?;
            let tab_idx = loaded_editor_tab(app, media_id)?;
            let position_ms = required_bounded_u64(&request.arguments, "position_ms", 86_400_000)?;
            let sample_rate = u64::from(app.tabs[tab_idx].buffer_sample_rate.max(1));
            let sample = position_ms.saturating_mul(sample_rate) / 1000;
            if sample >= app.tabs[tab_idx].samples_len as u64 {
                return Err(ActionError::Precondition(
                    "marker position is outside the loaded editor audio".into(),
                ));
            }
            if app.tabs[tab_idx]
                .markers
                .iter()
                .any(|marker| marker.sample as u64 == sample)
            {
                return Err(ActionError::Precondition(
                    "a marker already exists at this position".into(),
                ));
            }
            optional_bounded_string(&request.arguments, "label", 128).map(|_| ())
        }
        "labels.accept" => {
            let media_id = required_media_id(&request.arguments)?;
            let requested_class = required_audio_kind(&request.arguments)?;
            let item = app
                .item_for_id(media_id)
                .ok_or_else(|| ActionError::Precondition("MediaId no longer exists".into()))?;
            let suggestion = item
                .ai_metadata
                .as_deref()
                .and_then(|metadata| metadata.classification_suggestion.as_ref())
                .ok_or_else(|| {
                    ActionError::Precondition("AI suggestion no longer exists".into())
                })?;
            if suggestion.primary_class != requested_class {
                return Err(ActionError::Precondition(
                    "current AI suggestion differs from the requested class".into(),
                ));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn required_bounded_u64(arguments: &Value, key: &str, max: u64) -> Result<u64, ActionError> {
    arguments
        .get(key)
        .and_then(Value::as_u64)
        .filter(|value| *value <= max)
        .ok_or_else(|| {
            ActionError::InvalidArguments(format!("{key} must be an integer within 0..={max}"))
        })
}

fn optional_bounded_string(
    arguments: &Value,
    key: &str,
    max_chars: usize,
) -> Result<Option<String>, ActionError> {
    let Some(value) = arguments.get(key) else {
        return Ok(None);
    };
    let value = value.as_str().ok_or_else(|| {
        ActionError::InvalidArguments(format!("{key} must be a string when provided"))
    })?;
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max_chars {
        return Err(ActionError::InvalidArguments(format!(
            "{key} must contain 1..={max_chars} characters"
        )));
    }
    Ok(Some(value.to_string()))
}

fn loaded_editor_tab(app: &WavesPreviewer, media_id: MediaId) -> Result<usize, ActionError> {
    let item = app
        .item_for_id(media_id)
        .ok_or_else(|| ActionError::Precondition("MediaId no longer exists".into()))?;
    let tab_idx = app
        .tabs
        .iter()
        .position(|tab| tab.path == item.path)
        .ok_or_else(|| {
            ActionError::Precondition("target MediaId is not open in the Editor".into())
        })?;
    let tab = &app.tabs[tab_idx];
    if tab.loading || tab.samples_len == 0 {
        return Err(ActionError::Precondition(
            "target editor audio is still loading".into(),
        ));
    }
    Ok(tab_idx)
}

fn required_editor_range(
    app: &WavesPreviewer,
    tab_idx: usize,
    arguments: &Value,
) -> Result<(usize, usize), ActionError> {
    optional_editor_range(app, tab_idx, arguments)?.ok_or_else(|| {
        ActionError::InvalidArguments("start_ms and end_ms are required together".into())
    })
}

fn optional_editor_range(
    app: &WavesPreviewer,
    tab_idx: usize,
    arguments: &Value,
) -> Result<Option<(usize, usize)>, ActionError> {
    let start = arguments.get("start_ms");
    let end = arguments.get("end_ms");
    if start.is_none() && end.is_none() {
        return Ok(None);
    }
    if start.is_none() || end.is_none() {
        return Err(ActionError::InvalidArguments(
            "start_ms and end_ms must be provided together".into(),
        ));
    }
    let start_ms = required_bounded_u64(arguments, "start_ms", 86_400_000)?;
    let end_ms = required_bounded_u64(arguments, "end_ms", 86_400_000)?;
    if end_ms <= start_ms {
        return Err(ActionError::InvalidArguments(
            "end_ms must be greater than start_ms".into(),
        ));
    }
    let sample_rate = u64::from(app.tabs[tab_idx].buffer_sample_rate.max(1));
    let to_sample = |milliseconds: u64| {
        milliseconds
            .saturating_mul(sample_rate)
            .checked_div(1000)
            .unwrap_or(0) as usize
    };
    Ok(Some((to_sample(start_ms), to_sample(end_ms))))
}

fn validate_sample_range(
    tab: &super::super::types::EditorTab,
    range: (usize, usize),
) -> Result<(), ActionError> {
    if range.1 <= range.0 || range.1 > tab.samples_len {
        return Err(ActionError::Precondition(
            "requested range is outside the loaded editor audio".into(),
        ));
    }
    Ok(())
}

fn editor_mutation_result(
    tab: &super::super::types::EditorTab,
    media_id: MediaId,
    range: (usize, usize),
) -> Value {
    let sample_rate = u64::from(tab.buffer_sample_rate.max(1));
    json!({
        "media_id": media_id,
        "start_ms": (range.0 as u64).saturating_mul(1000) / sample_rate,
        "end_ms": (range.1 as u64).saturating_mul(1000) / sample_rate,
        "dirty": tab.dirty,
        "undo_available": !tab.undo_stack.is_empty(),
        "editor_revision": tab.viewport_source_generation
    })
}

fn required_media_ids(arguments: &Value) -> Result<Vec<MediaId>, ActionError> {
    let values = arguments
        .get("media_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| ActionError::InvalidArguments("media_ids must be an array".into()))?;
    if values.is_empty() || values.len() > 200 {
        return Err(ActionError::InvalidArguments(
            "media_ids must contain 1..=200 values".into(),
        ));
    }
    values
        .iter()
        .map(|value| {
            value.as_u64().filter(|id| *id > 0).ok_or_else(|| {
                ActionError::InvalidArguments("media_ids must contain positive integers".into())
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_id_arguments_are_bounded_and_positive() {
        assert_eq!(
            required_media_ids(&json!({"media_ids": [1, 2]})).unwrap(),
            vec![1, 2]
        );
        assert!(required_media_ids(&json!({"media_ids": []})).is_err());
        assert!(required_media_ids(&json!({"media_ids": [0]})).is_err());

        let registry = ActionRegistry::default();
        let descriptor = registry.get("list.query").unwrap();
        assert!(validate_arguments_contract(
            descriptor,
            &json!({"query": "door", "path": "C:/secret"})
        )
        .is_err());
        assert!(validate_arguments_contract(descriptor, &json!({"query": "door"})).is_ok());
    }

    #[test]
    fn label_acceptance_requires_bound_single_use_approval() {
        let path = std::env::temp_dir().join(format!(
            "neowaves_action_label_{}_{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        crate::wave::export_selection_wav(&[vec![0.0; 64]], 8_000, (0, 64), &path)
            .expect("write action fixture");
        let mut app = crate::app::WavesPreviewer::new_headless(crate::StartupConfig::default())
            .expect("headless app");
        app.replace_with_files(&[path.clone()]);
        let media_id = app.items[0].id;
        app.items[0].ai_metadata = Some(Box::new(crate::app::types::AiItemMetadata {
            classification_suggestion: Some(crate::ai::models::AudioClassification {
                primary_class: crate::ai::models::AudioKind::Se,
                confidence: 0.9,
                contains: Default::default(),
                description_ja: String::new(),
                warnings: Vec::new(),
            }),
            ..Default::default()
        }));
        let revision = app.action_context_snapshot().revision;
        let mut request = ActionRequest {
            action: "labels.accept".into(),
            arguments: json!({"media_id": media_id, "class": "se"}),
            expected_context_revision: Some(revision),
            approval: None,
        };
        let registry = ActionRegistry::default();
        let mut approvals = ApprovalStore::default();
        let approval = match dispatch_action(&mut app, &registry, &mut approvals, request.clone())
            .expect("plan label acceptance")
        {
            DispatchOutcome::NeedsApproval { request } => request,
            other => panic!("expected approval, got {other:?}"),
        };
        request.approval = Some(approvals.approve(approval.id).expect("approve plan"));
        let result = dispatch_action(&mut app, &registry, &mut approvals, request)
            .expect("execute approved label acceptance");
        assert!(matches!(result, DispatchOutcome::Completed { .. }));
        assert_eq!(
            app.items[0]
                .ai_metadata
                .as_deref()
                .and_then(|metadata| metadata.confirmed_class),
            Some(crate::ai::models::AudioKind::Se)
        );
        assert!(app.assistant_state.confirmed_metadata_dirty);
        drop(app);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn allowlisted_ui_controls_set_search_regex_and_press_clear() {
        let directory = std::env::temp_dir().join(format!(
            "neowaves_action_ui_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let colour = directory.join("colour_bass.wav");
        let voice = directory.join("voice.wav");
        crate::wave::export_selection_wav(&[vec![0.0; 64]], 8_000, (0, 64), &colour)
            .expect("write colour fixture");
        crate::wave::export_selection_wav(&[vec![0.0; 64]], 8_000, (0, 64), &voice)
            .expect("write voice fixture");
        let mut app = crate::app::WavesPreviewer::new_headless(crate::StartupConfig::default())
            .expect("headless app");
        app.replace_with_files(&[colour, voice]);
        let registry = ActionRegistry::default();
        let mut approvals = ApprovalStore::default();

        let set_text = ActionRequest {
            action: "ui.text.set".into(),
            arguments: json!({
                "control_id": "list.search.query",
                "text": "colour"
            }),
            expected_context_revision: None,
            approval: None,
        };
        assert!(matches!(
            dispatch_action(&mut app, &registry, &mut approvals, set_text).unwrap(),
            DispatchOutcome::Completed { .. }
        ));
        assert_eq!(app.search_query, "colour");
        assert_eq!(app.files.len(), 1);

        let set_regex = ActionRequest {
            action: "ui.checkbox.set".into(),
            arguments: json!({
                "control_id": "list.search.regex",
                "checked": true
            }),
            expected_context_revision: None,
            approval: None,
        };
        dispatch_action(&mut app, &registry, &mut approvals, set_regex).unwrap();
        assert!(app.search_use_regex);

        let clear = ActionRequest {
            action: "ui.button.press".into(),
            arguments: json!({"control_id": "list.search.clear"}),
            expected_context_revision: None,
            approval: None,
        };
        dispatch_action(&mut app, &registry, &mut approvals, clear).unwrap();
        assert!(app.search_query.is_empty());
        assert_eq!(app.files.len(), 2);

        let invalid = ActionRequest {
            action: "ui.button.press".into(),
            arguments: json!({"control_id": "filesystem.delete"}),
            expected_context_revision: None,
            approval: None,
        };
        assert!(dispatch_action(&mut app, &registry, &mut approvals, invalid).is_err());

        drop(app);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn editor_marker_approval_rejects_changed_editor_revision_then_supports_undo() {
        let path = std::env::temp_dir().join(format!(
            "neowaves_action_marker_{}_{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        crate::wave::export_selection_wav(&[vec![0.0; 8_000]], 8_000, (0, 8_000), &path)
            .expect("write editor action fixture");
        let mut app = crate::app::WavesPreviewer::new_headless(crate::StartupConfig::default())
            .expect("headless app");
        app.replace_with_files(&[path.clone()]);
        let media_id = app.items[0].id;
        let mut tab = crate::app::types::EditorTab::new_base(path.clone(), "marker fixture".into());
        tab.loading = false;
        tab.buffer_sample_rate = 8_000;
        tab.samples_len = 8_000;
        tab.samples_len_visual = 8_000;
        tab.ch_samples = vec![vec![0.0; 8_000]];
        tab.ch_samples_arc = std::sync::Arc::new(tab.ch_samples.clone());
        app.tabs.push(tab);
        app.active_tab = Some(0);
        app.workspace_view = crate::app::types::WorkspaceView::Editor;

        let revision = app.action_context_snapshot().revision;
        let base_request = ActionRequest {
            action: "markers.add".into(),
            arguments: json!({"media_id": media_id, "position_ms": 500, "label": "AI"}),
            expected_context_revision: Some(revision),
            approval: None,
        };
        let registry = ActionRegistry::default();
        let mut approvals = ApprovalStore::default();
        let first = match dispatch_action(&mut app, &registry, &mut approvals, base_request.clone())
            .expect("plan marker")
        {
            DispatchOutcome::NeedsApproval { request } => request,
            other => panic!("expected approval, got {other:?}"),
        };
        let mut stale_request = base_request.clone();
        stale_request.approval = Some(approvals.approve(first.id).expect("approve marker plan"));
        app.tabs[0].viewport_source_generation += 1;
        assert!(matches!(
            dispatch_action(&mut app, &registry, &mut approvals, stale_request),
            Err(ActionError::ApprovalInvalid(_))
        ));
        assert!(app.tabs[0].markers.is_empty());

        let second =
            match dispatch_action(&mut app, &registry, &mut approvals, base_request.clone())
                .expect("replan marker")
            {
                DispatchOutcome::NeedsApproval { request } => request,
                other => panic!("expected approval, got {other:?}"),
            };
        let mut approved_request = base_request;
        approved_request.approval =
            Some(approvals.approve(second.id).expect("approve current plan"));
        let outcome = dispatch_action(&mut app, &registry, &mut approvals, approved_request)
            .expect("execute marker");
        assert!(matches!(outcome, DispatchOutcome::Completed { .. }));
        assert_eq!(app.tabs[0].markers.len(), 1);
        assert_eq!(app.tabs[0].markers[0].label, "AI");
        assert!(!app.tabs[0].undo_stack.is_empty());
        drop(app);
        let _ = std::fs::remove_file(path);
    }
}
