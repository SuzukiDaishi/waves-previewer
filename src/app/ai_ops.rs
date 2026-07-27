#[cfg(feature = "gemini")]
use std::time::Instant;

#[cfg(feature = "gemini")]
use super::assistant_ops::{
    AssistantJobUi, AssistantMessage, AssistantMessageRole, AssistantTraceUi, PendingAgentApproval,
    SimilarityResultUi,
};
use super::WavesPreviewer;

impl WavesPreviewer {
    pub(super) fn drain_gemini_events(&mut self, ctx: &egui::Context) {
        #[cfg(feature = "gemini")]
        {
            let mut events = Vec::new();
            if let Some(runtime) = &self.assistant_runtime {
                for _ in 0..64 {
                    match runtime.try_recv() {
                        Ok(event) => events.push(event),
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            self.assistant_state.last_error =
                                Some("Gemini runtime stopped unexpectedly.".into());
                            self.assistant_runtime = None;
                            break;
                        }
                    }
                }
            }
            if events.is_empty() {
                return;
            }
            for event in events {
                self.apply_gemini_event(event);
            }
            ctx.request_repaint();
        }
        #[cfg(not(feature = "gemini"))]
        let _ = ctx;
    }

    #[cfg(feature = "gemini")]
    fn apply_gemini_event(&mut self, event: crate::ai::coordinator::AiEvent) {
        use crate::ai::coordinator::AiEvent;
        match event {
            AiEvent::Started { job_id, label, .. } => {
                self.assistant_state.active_jobs.insert(
                    job_id,
                    AssistantJobUi {
                        label,
                        started_at: Instant::now(),
                    },
                );
            }
            AiEvent::InteractionDelta { job_id, text } => {
                let partial = self
                    .assistant_state
                    .streaming_text
                    .entry(job_id)
                    .or_default();
                const MAX_STREAMED_TEXT_BYTES: usize = 256 * 1024;
                if partial.len() < MAX_STREAMED_TEXT_BYTES {
                    let remaining = MAX_STREAMED_TEXT_BYTES - partial.len();
                    if text.len() <= remaining {
                        partial.push_str(&text);
                    } else {
                        let mut boundary = remaining;
                        while boundary > 0 && !text.is_char_boundary(boundary) {
                            boundary -= 1;
                        }
                        partial.push_str(&text[..boundary]);
                    }
                }
            }
            AiEvent::InteractionCompleted { job_id, response } => {
                self.assistant_state.streaming_text.remove(&job_id);
                let elapsed_ms = self
                    .assistant_state
                    .active_jobs
                    .remove(&job_id)
                    .map(|job| job.started_at.elapsed().as_millis() as u64)
                    .unwrap_or(0);
                if !response.output_text.trim().is_empty() {
                    self.assistant_state.messages.push(AssistantMessage {
                        role: AssistantMessageRole::Assistant,
                        text: response.output_text.clone(),
                    });
                }
                self.assistant_state
                    .agent
                    .record_interaction(response.id.clone(), response.usage.total_tokens);

                if response.function_calls.is_empty() {
                    self.assistant_state.pending_chat_job = None;
                    self.assistant_state.traces.push(AssistantTraceUi {
                        action: "interaction".into(),
                        summary: format!("{} tokens", response.usage.total_tokens),
                        ok: true,
                        duration_ms: elapsed_ms,
                        target_count: 0,
                    });
                    return;
                }
                if self.assistant_state.privacy_mode {
                    self.assistant_state.pending_chat_job = None;
                    self.assistant_state.last_error =
                        Some("Agent Actions require stored conversation mode.".into());
                    return;
                }

                let registry = super::actions::ActionRegistry::default();
                let mut approvals = std::mem::take(&mut self.assistant_state.approvals);
                let mut tool_results = Vec::new();
                let mut pending_approval = None;
                for call in response.function_calls {
                    let started = Instant::now();
                    if !self.assistant_action_allowed(&call.name) {
                        let summary =
                            "Action blocked by the current context-chip privacy settings.";
                        self.assistant_state.traces.push(AssistantTraceUi {
                            action: call.name.clone(),
                            summary: summary.into(),
                            ok: false,
                            duration_ms: started.elapsed().as_millis() as u64,
                            target_count: 0,
                        });
                        tool_results.push(crate::ai::models::FunctionResult {
                            call_id: call.id,
                            name: call.name,
                            result: serde_json::json!({"error": summary}),
                            is_error: true,
                        });
                        continue;
                    }
                    let context_revision = self.action_context_snapshot().revision;
                    let action_arguments = call.arguments.clone();
                    let request = super::actions::ActionRequest {
                        action: call.name.clone(),
                        arguments: call.arguments,
                        expected_context_revision: Some(context_revision),
                        approval: None,
                    };
                    match super::actions::dispatch_action(self, &registry, &mut approvals, request)
                    {
                        Ok(super::actions::DispatchOutcome::Completed { result }) => {
                            let target_count = result.affected_media_ids.len();
                            self.assistant_state.traces.push(AssistantTraceUi {
                                action: call.name.clone(),
                                summary: result.summary.clone(),
                                ok: true,
                                duration_ms: started.elapsed().as_millis() as u64,
                                target_count,
                            });
                            tool_results.push(crate::ai::models::FunctionResult {
                                call_id: call.id,
                                name: call.name,
                                result: serde_json::to_value(result)
                                    .unwrap_or_else(|_| serde_json::json!({"ok": false})),
                                is_error: false,
                            });
                        }
                        Ok(super::actions::DispatchOutcome::NeedsApproval { request }) => {
                            self.assistant_state.traces.push(AssistantTraceUi {
                                action: call.name.clone(),
                                summary: "Waiting for explicit approval.".into(),
                                ok: false,
                                duration_ms: started.elapsed().as_millis() as u64,
                                target_count: 0,
                            });
                            if pending_approval.is_none() {
                                pending_approval = Some(PendingAgentApproval {
                                    approval: request,
                                    action_request: super::actions::ActionRequest {
                                        action: call.name.clone(),
                                        arguments: action_arguments,
                                        expected_context_revision: Some(context_revision),
                                        approval: None,
                                    },
                                    call_id: call.id,
                                    call_name: call.name,
                                    previous_interaction_id: response.id.clone(),
                                    completed_results: Vec::new(),
                                });
                            } else {
                                tool_results.push(crate::ai::models::FunctionResult {
                                    call_id: call.id,
                                    name: call.name,
                                    result: serde_json::json!({
                                        "error": "only one approval can be reviewed at a time"
                                    }),
                                    is_error: true,
                                });
                            }
                        }
                        Err(error) => {
                            self.assistant_state.traces.push(AssistantTraceUi {
                                action: call.name.clone(),
                                summary: error.to_string(),
                                ok: false,
                                duration_ms: started.elapsed().as_millis() as u64,
                                target_count: 0,
                            });
                            tool_results.push(crate::ai::models::FunctionResult {
                                call_id: call.id,
                                name: call.name,
                                result: serde_json::json!({"error": error.to_string()}),
                                is_error: true,
                            });
                        }
                    }
                }
                self.assistant_state.approvals = approvals;
                if let Some(mut pending) = pending_approval {
                    pending.completed_results = tool_results;
                    self.assistant_state.pending_agent_approval = Some(pending);
                    self.assistant_state.pending_chat_job = None;
                    self.assistant_state.show_review = true;
                    return;
                }
                if let Err(error) =
                    self.continue_gemini_with_tool_results(response.id, tool_results)
                {
                    self.assistant_state.pending_chat_job = None;
                    self.assistant_state.last_error = Some(error);
                }
            }
            AiEvent::AnalysisCompleted {
                job_id,
                media_id,
                result,
            } => {
                self.assistant_state.active_jobs.remove(&job_id);
                self.assistant_state.pending_chat_job = self
                    .assistant_state
                    .pending_chat_job
                    .filter(|id| *id != job_id);
                let mut summary = "AI analysis completed.".to_string();
                let mut show_review = false;
                if let Some(media_id) = media_id {
                    if let Some(item) = self.item_for_id_mut(media_id) {
                        let metadata = item.ai_metadata.get_or_insert_with(Default::default);
                        match result {
                            crate::ai::models::AiAnalysisResult::Classification(value) => {
                                let class = value.primary_class;
                                let confidence = value.confidence;
                                metadata.classification_suggestion = Some(value);
                                summary = format!(
                                    "Classification suggestion for MediaId {media_id}: \
                                     {class:?} ({:.0}%).",
                                    confidence * 100.0
                                );
                                show_review = true;
                            }
                            crate::ai::models::AiAnalysisResult::Ucs(value) => {
                                metadata.ucs_suggestion = Some(value);
                                summary = format!("UCS suggestion ready for MediaId {media_id}.");
                                show_review = true;
                            }
                            crate::ai::models::AiAnalysisResult::Voice(value)
                            | crate::ai::models::AiAnalysisResult::Lyrics(value) => {
                                metadata.transcript_suggestion = Some(value);
                                summary =
                                    format!("Transcript suggestion ready for MediaId {media_id}.");
                                show_review = true;
                            }
                            crate::ai::models::AiAnalysisResult::Embedding(value) => {
                                summary = format!(
                                    "Generated {}-dimension embedding for MediaId {media_id}.",
                                    value.dimensions
                                );
                            }
                        }
                    }
                }
                if show_review {
                    self.assistant_state.show_review = true;
                }
                self.assistant_state.messages.push(AssistantMessage {
                    role: AssistantMessageRole::System,
                    text: summary,
                });
            }
            AiEvent::Indexed {
                job_id,
                media_id,
                content_hash,
                cached,
            } => {
                self.assistant_state.active_jobs.remove(&job_id);
                self.assistant_state
                    .indexed_content_hashes
                    .insert(media_id, content_hash);
                let mut batch_summary = None;
                let was_batch =
                    self.assistant_state
                        .embedding_batch
                        .as_mut()
                        .is_some_and(|batch| {
                            if !batch.job_ids.remove(&job_id) {
                                return false;
                            }
                            batch.completed += 1;
                            if cached {
                                batch.cached += 1;
                            } else {
                                batch.embedded += 1;
                            }
                            if batch.job_ids.is_empty() {
                                batch_summary = Some(format!(
                                "Embedding batch complete: {} file(s), {} unchanged cache hit(s), \
                                 {} new or changed, {} failed.",
                                batch.total, batch.cached, batch.embedded, batch.failed
                            ));
                            }
                            true
                        });
                if let Some(text) = batch_summary {
                    self.assistant_state.messages.push(AssistantMessage {
                        role: AssistantMessageRole::System,
                        text,
                    });
                } else if !was_batch {
                    self.assistant_state.messages.push(AssistantMessage {
                        role: AssistantMessageRole::System,
                        text: format!(
                            "Embedding indexed for MediaId {media_id}{}.",
                            if cached { " (cache hit)" } else { "" }
                        ),
                    });
                }
            }
            AiEvent::SimilarityCompleted {
                job_id,
                query_media_id,
                hits,
                indexed_segments,
                backend,
                warning,
            } => {
                self.assistant_state.active_jobs.remove(&job_id);
                if let Some(warning) = warning {
                    self.assistant_state.messages.push(AssistantMessage {
                        role: AssistantMessageRole::System,
                        text: warning,
                    });
                }
                let mut results = Vec::new();
                for hit in hits {
                    for path in &hit.path_keys {
                        if let Some(media_id) = self.path_index.get(&std::path::PathBuf::from(path))
                        {
                            results.push(SimilarityResultUi {
                                media_id,
                                score: hit.score,
                                segment_start_ms: hit.segment_start_ms,
                                segment_end_ms: hit.segment_end_ms,
                            });
                        }
                    }
                    for (&media_id, content_hash) in &self.assistant_state.indexed_content_hashes {
                        if content_hash == &hit.content_hash
                            && !results.iter().any(|result| {
                                result.media_id == media_id
                                    && result.segment_start_ms == hit.segment_start_ms
                                    && result.segment_end_ms == hit.segment_end_ms
                            })
                        {
                            results.push(SimilarityResultUi {
                                media_id,
                                score: hit.score,
                                segment_start_ms: hit.segment_start_ms,
                                segment_end_ms: hit.segment_end_ms,
                            });
                        }
                    }
                }
                results.sort_by(|left, right| {
                    right
                        .score
                        .partial_cmp(&left.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                self.assistant_state.similarity_reference_media_id = query_media_id;
                self.assistant_state.similarity_results = results;
                self.assistant_state.messages.push(AssistantMessage {
                    role: AssistantMessageRole::System,
                    text: format!(
                        "{} {indexed_segments} indexed audio segment(s) with {}.",
                        if query_media_id.is_some() {
                            "Compared against"
                        } else {
                            "Searched"
                        },
                        backend.as_str(),
                    ),
                });
                self.assistant_state.show_review = true;
            }
            AiEvent::Failed { job_id, error, .. } => {
                self.assistant_state.streaming_text.remove(&job_id);
                self.assistant_state.active_jobs.remove(&job_id);
                self.assistant_state.pending_chat_job = self
                    .assistant_state
                    .pending_chat_job
                    .filter(|id| *id != job_id);
                let mut batch_summary = None;
                let was_batch =
                    self.assistant_state
                        .embedding_batch
                        .as_mut()
                        .is_some_and(|batch| {
                            if !batch.job_ids.remove(&job_id) {
                                return false;
                            }
                            batch.completed += 1;
                            batch.failed += 1;
                            batch.last_error = Some(error.clone());
                            if batch.job_ids.is_empty() {
                                batch_summary = Some(format!(
                                "Embedding batch complete: {} file(s), {} unchanged cache hit(s), \
                                 {} new or changed, {} failed. Last error: {}",
                                batch.total, batch.cached, batch.embedded, batch.failed, error
                            ));
                            }
                            true
                        });
                if let Some(text) = batch_summary {
                    self.assistant_state.last_error = Some(text.clone());
                    self.assistant_state.messages.push(AssistantMessage {
                        role: AssistantMessageRole::System,
                        text,
                    });
                } else if !was_batch {
                    self.assistant_state.last_error = Some(error);
                }
            }
            AiEvent::Cancelled { job_id } => {
                self.assistant_state.streaming_text.remove(&job_id);
                self.assistant_state.active_jobs.remove(&job_id);
                self.assistant_state.pending_chat_job = self
                    .assistant_state
                    .pending_chat_job
                    .filter(|id| *id != job_id);
                let mut batch_summary = None;
                let was_batch =
                    self.assistant_state
                        .embedding_batch
                        .as_mut()
                        .is_some_and(|batch| {
                            if !batch.job_ids.remove(&job_id) {
                                return false;
                            }
                            batch.completed += 1;
                            batch.failed += 1;
                            batch.last_error = Some("Cancelled".into());
                            if batch.job_ids.is_empty() {
                                batch_summary = Some(format!(
                                    "Embedding batch cancelled: {} completed, {} cache hit(s), \
                                 {} new or changed, {} cancelled or failed.",
                                    batch.completed, batch.cached, batch.embedded, batch.failed
                                ));
                            }
                            true
                        });
                if let Some(text) = batch_summary {
                    self.assistant_state.messages.push(AssistantMessage {
                        role: AssistantMessageRole::System,
                        text,
                    });
                } else if !was_batch {
                    self.assistant_state.messages.push(AssistantMessage {
                        role: AssistantMessageRole::System,
                        text: "Gemini request cancelled.".into(),
                    });
                }
            }
        }
    }

    #[cfg(feature = "gemini")]
    pub(super) fn approve_pending_gemini_action(&mut self) {
        let Some(mut pending) = self.assistant_state.pending_agent_approval.take() else {
            return;
        };
        let mut approvals = std::mem::take(&mut self.assistant_state.approvals);
        let result = approvals.approve(pending.approval.id).and_then(|token| {
            pending.action_request.approval = Some(token);
            super::actions::dispatch_action(
                self,
                &super::actions::ActionRegistry::default(),
                &mut approvals,
                pending.action_request,
            )
        });
        self.assistant_state.approvals = approvals;
        let function_result = match result {
            Ok(super::actions::DispatchOutcome::Completed { result }) => {
                self.assistant_state.traces.push(AssistantTraceUi {
                    action: pending.call_name.clone(),
                    summary: result.summary.clone(),
                    ok: true,
                    duration_ms: 0,
                    target_count: result.affected_media_ids.len(),
                });
                crate::ai::models::FunctionResult {
                    call_id: pending.call_id,
                    name: pending.call_name,
                    result: serde_json::to_value(result)
                        .unwrap_or_else(|_| serde_json::json!({"ok": false})),
                    is_error: false,
                }
            }
            Ok(super::actions::DispatchOutcome::NeedsApproval { .. }) => {
                crate::ai::models::FunctionResult {
                    call_id: pending.call_id,
                    name: pending.call_name,
                    result: serde_json::json!({"error": "approval token was not accepted"}),
                    is_error: true,
                }
            }
            Err(error) => {
                self.assistant_state.last_error = Some(error.to_string());
                crate::ai::models::FunctionResult {
                    call_id: pending.call_id,
                    name: pending.call_name,
                    result: serde_json::json!({"error": error.to_string()}),
                    is_error: true,
                }
            }
        };
        pending.completed_results.push(function_result);
        if let Err(error) = self.continue_gemini_with_tool_results(
            pending.previous_interaction_id,
            pending.completed_results,
        ) {
            self.assistant_state.last_error = Some(error);
        }
    }

    #[cfg(feature = "gemini")]
    pub(super) fn reject_pending_gemini_action(&mut self) {
        let Some(mut pending) = self.assistant_state.pending_agent_approval.take() else {
            return;
        };
        self.assistant_state.approvals.reject(pending.approval.id);
        pending
            .completed_results
            .push(crate::ai::models::FunctionResult {
                call_id: pending.call_id,
                name: pending.call_name,
                result: serde_json::json!({"error": "user_rejected_approval"}),
                is_error: true,
            });
        if let Err(error) = self.continue_gemini_with_tool_results(
            pending.previous_interaction_id,
            pending.completed_results,
        ) {
            self.assistant_state.last_error = Some(error);
        }
    }
}
