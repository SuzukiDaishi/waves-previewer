use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::plugin::backends::{clap, vst3};
use crate::plugin::protocol::{
    GuiCapabilities, PluginHostBackend, WorkerRequest, WorkerResponse,
};

enum GuiSession {
    Vst3(vst3::GuiSession),
    Clap(clap::GuiSession),
}

struct SessionEntry {
    backend: PluginHostBackend,
    state_blob_b64: Option<String>,
    capabilities: GuiCapabilities,
    session: GuiSession,
}

fn plugin_format(path: &Path) -> Option<crate::plugin::PluginFormat> {
    crate::plugin::backends::resolve_plugin_format(path)
}

fn as_gui_error(session_id: u64, message: impl Into<String>) -> WorkerResponse {
    WorkerResponse::GuiError {
        session_id,
        message: message.into(),
    }
}

pub struct GuiWorkerService {
    sessions: HashMap<u64, SessionEntry>,
}

impl GuiWorkerService {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
        }
    }

    pub fn handle_request(&mut self, request: WorkerRequest) -> WorkerResponse {
        match request {
            WorkerRequest::Ping => WorkerResponse::Pong,
            WorkerRequest::GuiSessionOpen {
                session_id,
                plugin_path,
                state_blob_b64,
                params,
            } => {
                if self.sessions.contains_key(&session_id) {
                    return as_gui_error(session_id, "GUI session already exists");
                }
                let path = PathBuf::from(&plugin_path);
                let Some(format) = plugin_format(&path) else {
                    return as_gui_error(
                        session_id,
                        format!("unsupported plugin format: {}", path.display()),
                    );
                };
                match format {
                    crate::plugin::PluginFormat::Vst3 => {
                        let capabilities = vst3::gui_capabilities(&path);
                        match vst3::gui_open(&path, state_blob_b64.as_deref(), &params) {
                            Ok((session, probe_params, backend_note)) => {
                                self.sessions.insert(
                                    session_id,
                                    SessionEntry {
                                        backend: PluginHostBackend::NativeVst3,
                                        state_blob_b64: state_blob_b64.clone(),
                                        capabilities,
                                        session: GuiSession::Vst3(session),
                                    },
                                );
                                WorkerResponse::GuiOpened {
                                    session_id,
                                    backend: PluginHostBackend::NativeVst3,
                                    params: probe_params,
                                    state_blob_b64,
                                    capabilities,
                                    backend_note,
                                }
                            }
                            Err(err) => as_gui_error(session_id, err),
                        }
                    }
                    crate::plugin::PluginFormat::Clap => {
                        let capabilities = clap::gui_capabilities(&path);
                        match clap::gui_open(&path, state_blob_b64.as_deref(), &params) {
                            Ok((session, probe_params, backend_note)) => {
                                self.sessions.insert(
                                    session_id,
                                    SessionEntry {
                                        backend: PluginHostBackend::NativeClap,
                                        state_blob_b64: state_blob_b64.clone(),
                                        capabilities,
                                        session: GuiSession::Clap(session),
                                    },
                                );
                                WorkerResponse::GuiOpened {
                                    session_id,
                                    backend: PluginHostBackend::NativeClap,
                                    params: probe_params,
                                    state_blob_b64,
                                    capabilities,
                                    backend_note,
                                }
                            }
                            Err(err) => as_gui_error(session_id, err),
                        }
                    }
                }
            }
            WorkerRequest::GuiSessionPoll { session_id } => {
                let Some(entry) = self.sessions.get_mut(&session_id) else {
                    return as_gui_error(session_id, "GUI session not found");
                };
                let backend = entry.backend;
                let caps = entry.capabilities;
                let poll_result = match &mut entry.session {
                    GuiSession::Vst3(session) => vst3::gui_poll(session),
                    GuiSession::Clap(session) => clap::gui_poll(session),
                };
                match poll_result {
                    Ok((deltas, snapshot, state_blob_b64, closed)) => {
                        if let Some(next) = state_blob_b64 {
                            entry.state_blob_b64 = Some(next);
                        }
                        let snapshot_params = snapshot.unwrap_or(deltas);
                        let response = WorkerResponse::GuiSnapshot {
                            session_id,
                            params: snapshot_params,
                            state_blob_b64: entry.state_blob_b64.clone(),
                            backend,
                            closed,
                            backend_note: if caps.supports_native_gui {
                                None
                            } else {
                                Some("native GUI unsupported; running in stub mode".to_string())
                            },
                        };
                        if closed {
                            if let Some(done) = self.sessions.remove(&session_id) {
                                let _ = match done.session {
                                    GuiSession::Vst3(session) => vst3::gui_close(session),
                                    GuiSession::Clap(session) => clap::gui_close(session),
                                };
                            }
                        }
                        response
                    }
                    Err(err) => as_gui_error(session_id, err),
                }
            }
            WorkerRequest::GuiSessionClose { session_id } => {
                let Some(entry) = self.sessions.remove(&session_id) else {
                    return WorkerResponse::GuiClosed {
                        session_id,
                        state_blob_b64: None,
                        backend: PluginHostBackend::Generic,
                        backend_note: Some("GUI session not found".to_string()),
                    };
                };
                let close_result = match entry.session {
                    GuiSession::Vst3(session) => vst3::gui_close(session),
                    GuiSession::Clap(session) => clap::gui_close(session),
                };
                match close_result {
                    Ok((_snapshot, state_blob_b64)) => WorkerResponse::GuiClosed {
                        session_id,
                        state_blob_b64: state_blob_b64.or(entry.state_blob_b64),
                        backend: entry.backend,
                        backend_note: None,
                    },
                    Err(err) => as_gui_error(session_id, err),
                }
            }
            _ => WorkerResponse::Error {
                message: "unsupported request for GUI worker".to_string(),
            },
        }
    }
}
