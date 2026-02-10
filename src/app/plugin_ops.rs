use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

use crate::app::types::{
    PluginCatalogEntry, PluginGuiCommand, PluginGuiEvent, PluginGuiSessionState, PluginParamUiState,
    PluginProbeResult, PluginProbeState, PluginProcessResult, PluginProcessState, PluginScanResult,
    PluginScanState, ToolKind,
};
use crate::plugin::{PluginParamValue, WorkerRequest, WorkerResponse};
use base64::Engine;

const PLUGIN_METRIC_CAP: usize = 256;
const PLUGIN_GUI_POLL_MS: u64 = 16;

impl crate::app::WavesPreviewer {
    fn native_probe_fallback_hint(plugin_key: &str, params_empty: bool) -> Option<String> {
        let ext = Path::new(plugin_key)
            .extension()
            .and_then(|v| v.to_str())
            .map(|v| v.to_ascii_lowercase());
        match ext.as_deref() {
            Some("vst3") => {
                if !cfg!(feature = "plugin_native_vst3") {
                    return Some(
                        "native VST3 backend is disabled (build with --features plugin_native_vst3)"
                            .to_string(),
                    );
                }
                if params_empty {
                    return Some(
                        "native VST3 probe failed for this plugin; generic fallback is active"
                            .to_string(),
                    );
                }
                None
            }
            Some("clap") => {
                if !cfg!(feature = "plugin_native_clap") {
                    return Some(
                        "native CLAP backend is disabled (build with --features plugin_native_clap)"
                            .to_string(),
                    );
                }
                if params_empty {
                    return Some(
                        "native CLAP probe failed for this plugin; generic fallback is active"
                            .to_string(),
                    );
                }
                None
            }
            _ => None,
        }
    }

    fn plugin_path_key(path: &Path) -> String {
        #[cfg(windows)]
        {
            path.to_string_lossy().replace('\\', "/").to_ascii_lowercase()
        }
        #[cfg(not(windows))]
        {
            path.to_string_lossy().replace('\\', "/")
        }
    }

    pub(super) fn normalize_plugin_search_paths(paths: &mut Vec<PathBuf>) {
        let mut cleaned = Vec::new();
        let mut seen = std::collections::HashSet::<String>::new();
        for raw in paths.drain(..) {
            let text = raw.to_string_lossy();
            let trimmed = text.trim().trim_matches('"');
            if trimmed.is_empty() {
                continue;
            }
            let path = PathBuf::from(trimmed);
            let key = Self::plugin_path_key(&path);
            if seen.insert(key) {
                cleaned.push(path);
            }
        }
        *paths = cleaned;
    }

    fn is_native_plugin_path(path: &Path) -> bool {
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| {
                let ext = ext.to_ascii_lowercase();
                ext == "vst3" || ext == "clap"
            })
            .unwrap_or(false)
    }

    fn is_native_plugin_key(key: &str) -> bool {
        Self::is_native_plugin_path(Path::new(key))
    }

    pub(super) fn default_plugin_search_paths() -> Vec<PathBuf> {
        let mut out = Vec::new();
        #[cfg(windows)]
        {
            if let Ok(local) = std::env::var("LOCALAPPDATA") {
                out.push(PathBuf::from(&local).join("Programs").join("Common").join("VST3"));
                out.push(PathBuf::from(local).join("Programs").join("Common").join("CLAP"));
            }
            if let Ok(common) = std::env::var("COMMONPROGRAMFILES") {
                out.push(PathBuf::from(&common).join("VST3"));
                out.push(PathBuf::from(common).join("CLAP"));
            }
            if let Ok(program_files) = std::env::var("PROGRAMFILES") {
                out.push(
                    PathBuf::from(&program_files)
                        .join("Common Files")
                        .join("VST3"),
                );
                out.push(
                    PathBuf::from(program_files)
                        .join("Common Files")
                        .join("CLAP"),
                );
            }
            if let Ok(program_files_x86) = std::env::var("PROGRAMFILES(X86)") {
                out.push(
                    PathBuf::from(program_files_x86)
                        .join("Common Files")
                        .join("VST3"),
                );
            }
            if let Ok(exe) = std::env::current_exe() {
                if let Some(app_dir) = exe.parent() {
                    out.push(app_dir.join("VST3"));
                    out.push(app_dir.join("CLAP"));
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            if let Ok(home) = std::env::var("HOME") {
                out.push(
                    PathBuf::from(&home)
                        .join("Library")
                        .join("Audio")
                        .join("Plug-Ins")
                        .join("VST3"),
                );
                out.push(
                    PathBuf::from(&home)
                        .join("Library")
                        .join("Audio")
                        .join("Plug-Ins")
                        .join("CLAP"),
                );
            }
            out.push(
                PathBuf::from("/Library")
                    .join("Audio")
                    .join("Plug-Ins")
                    .join("VST3"),
            );
            out.push(
                PathBuf::from("/Library")
                    .join("Audio")
                    .join("Plug-Ins")
                    .join("CLAP"),
            );
            out.push(
                PathBuf::from("/Network")
                    .join("Library")
                    .join("Audio")
                    .join("Plug-Ins")
                    .join("VST3"),
            );
            if let Ok(exe) = std::env::current_exe() {
                if let Some(macos_dir) = exe.parent() {
                    if let Some(contents_dir) = macos_dir.parent() {
                        out.push(contents_dir.join("VST3"));
                        out.push(contents_dir.join("CLAP"));
                    }
                }
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            if let Ok(home) = std::env::var("HOME") {
                out.push(PathBuf::from(&home).join(".vst3"));
                out.push(PathBuf::from(home).join(".clap"));
            }
            out.push(PathBuf::from("/usr/lib/vst3"));
            out.push(PathBuf::from("/usr/lib/clap"));
            out.push(PathBuf::from("/usr/local/lib/vst3"));
            out.push(PathBuf::from("/usr/local/lib/clap"));
            if let Ok(exe) = std::env::current_exe() {
                if let Some(app_dir) = exe.parent() {
                    out.push(app_dir.join("vst3"));
                    out.push(app_dir.join("clap"));
                }
            }
        }
        if let Some(clap_path) = std::env::var_os("CLAP_PATH") {
            out.extend(std::env::split_paths(&clap_path));
        }
        Self::normalize_plugin_search_paths(&mut out);
        out
    }

    pub(super) fn add_plugin_search_path(&mut self, path: PathBuf) -> bool {
        let mut merged = self.plugin_search_paths.clone();
        merged.push(path);
        let before = self.plugin_search_paths.len();
        Self::normalize_plugin_search_paths(&mut merged);
        let changed = merged != self.plugin_search_paths;
        if changed || merged.len() != before {
            self.plugin_search_paths = merged;
        }
        changed
    }

    pub(super) fn remove_plugin_search_path_at(&mut self, index: usize) -> bool {
        if index >= self.plugin_search_paths.len() {
            return false;
        }
        self.plugin_search_paths.remove(index);
        true
    }

    pub(super) fn reset_plugin_search_paths_to_default(&mut self) {
        self.plugin_search_paths = Self::default_plugin_search_paths();
    }

    fn plugin_next_job_id(&mut self) -> u64 {
        self.plugin_job_id = self.plugin_job_id.wrapping_add(1).max(1);
        self.plugin_job_id
    }

    fn plugin_push_metric(samples: &mut VecDeque<f32>, value_ms: f32) {
        if !value_ms.is_finite() {
            return;
        }
        samples.push_back(value_ms.max(0.0));
        while samples.len() > PLUGIN_METRIC_CAP {
            samples.pop_front();
        }
    }

    fn plugin_temp_paths(&mut self, job_id: u64) -> Result<(PathBuf, PathBuf), String> {
        let dir = std::env::temp_dir().join("neowaves_pluginfx");
        std::fs::create_dir_all(&dir).map_err(|e| format!("plugin temp dir failed: {e}"))?;
        self.plugin_temp_seq = self.plugin_temp_seq.wrapping_add(1);
        let seq = self.plugin_temp_seq;
        let input = dir.join(format!("plugin_in_{job_id}_{seq}.wav"));
        let output = dir.join(format!("plugin_out_{job_id}_{seq}.wav"));
        Ok((input, output))
    }

    fn plugin_key_to_path(&self, key: &str) -> PathBuf {
        if let Some(entry) = self.plugin_catalog.iter().find(|p| p.key == key) {
            return entry.path.clone();
        }
        PathBuf::from(key)
    }

    fn join_backend_log_lines(lines: Vec<String>) -> Option<String> {
        let merged = lines
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if merged.is_empty() {
            None
        } else {
            Some(merged)
        }
    }

    pub(super) fn request_plugin_scan_if_needed(&mut self) {
        if self.plugin_scan_error.is_some() {
            return;
        }
        if self.plugin_catalog.is_empty() && self.plugin_scan_state.is_none() {
            self.spawn_plugin_scan();
        }
    }

    pub(super) fn spawn_plugin_scan(&mut self) {
        if self.plugin_scan_state.is_some() {
            return;
        }
        self.plugin_scan_error = None;
        let job_id = self.plugin_next_job_id();
        let search_paths: Vec<String> = self
            .plugin_search_paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        let (tx, rx) = mpsc::channel::<PluginScanResult>();
        std::thread::spawn(move || {
            let req = WorkerRequest::Scan { search_paths };
            let result = match crate::plugin::client::run_request(&req) {
                Ok(WorkerResponse::ScanResult { plugins }) => {
                    let mut entries: Vec<PluginCatalogEntry> = plugins
                        .into_iter()
                        .map(|p| PluginCatalogEntry {
                            key: p.key,
                            name: p.name,
                            path: PathBuf::from(p.path),
                            format: p.format,
                        })
                        .collect();
                    entries.sort_by(|a, b| a.name.cmp(&b.name).then(a.path.cmp(&b.path)));
                    entries.dedup_by(|a, b| a.key == b.key);
                    PluginScanResult {
                        job_id,
                        plugins: entries,
                        error: None,
                    }
                }
                Ok(WorkerResponse::Error { message }) => PluginScanResult {
                    job_id,
                    plugins: Vec::new(),
                    error: Some(message),
                },
                Ok(_) => PluginScanResult {
                    job_id,
                    plugins: Vec::new(),
                    error: Some("plugin scan: unexpected worker response".to_string()),
                },
                Err(err) => PluginScanResult {
                    job_id,
                    plugins: Vec::new(),
                    error: Some(err),
                },
            };
            let _ = tx.send(result);
        });
        self.plugin_scan_state = Some(PluginScanState {
            job_id,
            started_at: Instant::now(),
            rx,
        });
    }

    pub(super) fn spawn_plugin_probe_for_tab(&mut self, tab_idx: usize, plugin_key: String) {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let tab_path = tab.path.clone();
        let plugin_path = self.plugin_key_to_path(&plugin_key);
        if !plugin_path.exists() {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                tab.plugin_fx_draft.last_error =
                    Some(format!("plugin not found: {}", plugin_path.display()));
            }
            return;
        }
        let job_id = self.plugin_next_job_id();
        let (tx, rx) = mpsc::channel::<PluginProbeResult>();
        let plugin_path_text = plugin_path.to_string_lossy().to_string();
        std::thread::spawn(move || {
            let req = WorkerRequest::Probe {
                plugin_path: plugin_path_text,
            };
            let result = match crate::plugin::client::run_request(&req) {
                Ok(WorkerResponse::ProbeResult {
                    plugin,
                    params,
                    state_blob_b64,
                    backend,
                    capabilities,
                    backend_note,
                }) => {
                    let ui_params: Vec<PluginParamUiState> = params
                        .into_iter()
                        .map(|p| PluginParamUiState {
                            id: p.id,
                            name: p.name,
                            normalized: p.normalized.clamp(0.0, 1.0),
                            default_normalized: p.default_normalized.clamp(0.0, 1.0),
                            min: p.min,
                            max: p.max,
                            unit: p.unit,
                        })
                        .collect();
                    PluginProbeResult {
                        job_id,
                        plugin_key: plugin.key,
                        plugin_name: plugin.name,
                        params: ui_params,
                        state_blob: state_blob_b64.and_then(|raw| {
                            base64::engine::general_purpose::STANDARD_NO_PAD
                                .decode(raw.as_bytes())
                                .ok()
                        }),
                        backend,
                        capabilities,
                        backend_note,
                        error: None,
                    }
                }
                Ok(WorkerResponse::Error { message }) => PluginProbeResult {
                    job_id,
                    plugin_key,
                    plugin_name: String::new(),
                    params: Vec::new(),
                    state_blob: None,
                    backend: crate::plugin::PluginHostBackend::Generic,
                    capabilities: crate::plugin::GuiCapabilities::default(),
                    backend_note: None,
                    error: Some(message),
                },
                Ok(_) => PluginProbeResult {
                    job_id,
                    plugin_key,
                    plugin_name: String::new(),
                    params: Vec::new(),
                    state_blob: None,
                    backend: crate::plugin::PluginHostBackend::Generic,
                    capabilities: crate::plugin::GuiCapabilities::default(),
                    backend_note: None,
                    error: Some("plugin probe: unexpected worker response".to_string()),
                },
                Err(err) => PluginProbeResult {
                    job_id,
                    plugin_key,
                    plugin_name: String::new(),
                    params: Vec::new(),
                    state_blob: None,
                    backend: crate::plugin::PluginHostBackend::Generic,
                    capabilities: crate::plugin::GuiCapabilities::default(),
                    backend_note: None,
                    error: Some(err),
                },
            };
            let _ = tx.send(result);
        });
        self.plugin_probe_state = Some(PluginProbeState {
            job_id,
            tab_path,
            started_at: Instant::now(),
            rx,
        });
    }

    fn draft_params_to_worker(params: &[PluginParamUiState]) -> Vec<PluginParamValue> {
        params
            .iter()
            .map(|p| PluginParamValue {
                id: p.id.clone(),
                normalized: p.normalized.clamp(0.0, 1.0),
            })
            .collect()
    }

    pub(super) fn open_plugin_gui_for_tab(&mut self, tab_idx: usize) {
        if let Some(state) = self.plugin_gui_state.take() {
            let _ = state.cmd_tx.send(PluginGuiCommand::Close);
        }
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let plugin_key_opt = tab.plugin_fx_draft.plugin_key.clone();
        let tab_path = tab.path.clone();
        let params = Self::draft_params_to_worker(&tab.plugin_fx_draft.params);
        let state_blob_b64 = tab
            .plugin_fx_draft
            .state_blob
            .as_ref()
            .map(|bytes| base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes));
        let Some(plugin_key) = plugin_key_opt else {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                tab.plugin_fx_draft.last_error = Some("plugin not selected".to_string());
            }
            return;
        };
        let plugin_path = self.plugin_key_to_path(&plugin_key);
        if !plugin_path.exists() {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                tab.plugin_fx_draft.last_error =
                    Some(format!("plugin not found: {}", plugin_path.display()));
            }
            return;
        }
        let session_id = self.plugin_next_job_id();
        let (cmd_tx, cmd_rx) = mpsc::channel::<PluginGuiCommand>();
        let (evt_tx, evt_rx) = mpsc::channel::<PluginGuiEvent>();
        let plugin_path_text = plugin_path.to_string_lossy().to_string();
        std::thread::spawn(move || {
            let mut client = match crate::plugin::client::GuiWorkerClient::spawn() {
                Ok(client) => client,
                Err(err) => {
                    let _ = evt_tx.send(PluginGuiEvent::Error {
                        session_id,
                        message: err,
                    });
                    return;
                }
            };
            let open_req = WorkerRequest::GuiSessionOpen {
                session_id,
                plugin_path: plugin_path_text,
                state_blob_b64,
                params,
            };
            match client.request(&open_req) {
                Ok(WorkerResponse::GuiOpened {
                    session_id,
                    backend,
                    params,
                    state_blob_b64,
                    capabilities,
                    backend_note,
                }) => {
                    let ui_params: Vec<PluginParamUiState> = params
                        .into_iter()
                        .map(|p| PluginParamUiState {
                            id: p.id,
                            name: p.name,
                            normalized: p.normalized.clamp(0.0, 1.0),
                            default_normalized: p.default_normalized.clamp(0.0, 1.0),
                            min: p.min,
                            max: p.max,
                            unit: p.unit,
                        })
                        .collect();
                    let state_blob = state_blob_b64.and_then(|raw| {
                        base64::engine::general_purpose::STANDARD_NO_PAD
                            .decode(raw.as_bytes())
                            .ok()
                    });
                    let _ = evt_tx.send(PluginGuiEvent::Opened {
                        session_id,
                        backend,
                        capabilities,
                        params: ui_params,
                        state_blob,
                        backend_note,
                    });
                }
                Ok(WorkerResponse::GuiError { session_id, message }) => {
                    let _ = evt_tx.send(PluginGuiEvent::Error { session_id, message });
                    client.close();
                    return;
                }
                Ok(WorkerResponse::Error { message }) => {
                    let _ = evt_tx.send(PluginGuiEvent::Error {
                        session_id,
                        message,
                    });
                    client.close();
                    return;
                }
                Ok(other) => {
                    let _ = evt_tx.send(PluginGuiEvent::Error {
                        session_id,
                        message: format!("gui open: unexpected response: {other:?}"),
                    });
                    client.close();
                    return;
                }
                Err(err) => {
                    let _ = evt_tx.send(PluginGuiEvent::Error {
                        session_id,
                        message: err,
                    });
                    client.close();
                    return;
                }
            }

            let mut running = true;
            while running {
                let mut should_poll = true;
                match cmd_rx.recv_timeout(std::time::Duration::from_millis(PLUGIN_GUI_POLL_MS)) {
                    Ok(PluginGuiCommand::Close) => {
                        should_poll = false;
                        let close_req = WorkerRequest::GuiSessionClose { session_id };
                        match client.request(&close_req) {
                            Ok(WorkerResponse::GuiClosed {
                                session_id,
                                state_blob_b64,
                                backend,
                                backend_note,
                            }) => {
                                let state_blob = state_blob_b64.and_then(|raw| {
                                    base64::engine::general_purpose::STANDARD_NO_PAD
                                        .decode(raw.as_bytes())
                                        .ok()
                                });
                                let _ = evt_tx.send(PluginGuiEvent::Closed {
                                    session_id,
                                    state_blob,
                                    backend,
                                    backend_note,
                                });
                            }
                            Ok(WorkerResponse::GuiError { session_id, message }) => {
                                let _ = evt_tx.send(PluginGuiEvent::Error { session_id, message });
                            }
                            Ok(WorkerResponse::Error { message }) => {
                                let _ = evt_tx.send(PluginGuiEvent::Error {
                                    session_id,
                                    message,
                                });
                            }
                            Ok(other) => {
                                let _ = evt_tx.send(PluginGuiEvent::Error {
                                    session_id,
                                    message: format!("gui close: unexpected response: {other:?}"),
                                });
                            }
                            Err(err) => {
                                let _ = evt_tx.send(PluginGuiEvent::Error {
                                    session_id,
                                    message: err,
                                });
                            }
                        }
                        running = false;
                    }
                    Ok(PluginGuiCommand::SyncNow) => {}
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        running = false;
                        should_poll = false;
                    }
                }
                if !running || !should_poll {
                    continue;
                }
                let poll_req = WorkerRequest::GuiSessionPoll { session_id };
                match client.request(&poll_req) {
                    Ok(WorkerResponse::GuiSnapshot {
                        session_id,
                        params,
                        state_blob_b64,
                        backend,
                        closed,
                        backend_note,
                    }) => {
                        let state_blob = state_blob_b64.and_then(|raw| {
                            base64::engine::general_purpose::STANDARD_NO_PAD
                                .decode(raw.as_bytes())
                                .ok()
                        });
                        let _ = evt_tx.send(PluginGuiEvent::Snapshot {
                            session_id,
                            params,
                            state_blob,
                            backend,
                            closed,
                            backend_note,
                        });
                        if closed {
                            running = false;
                        }
                    }
                    Ok(WorkerResponse::GuiError { session_id, message }) => {
                        let _ = evt_tx.send(PluginGuiEvent::Error { session_id, message });
                        running = false;
                    }
                    Ok(WorkerResponse::Error { message }) => {
                        let _ = evt_tx.send(PluginGuiEvent::Error {
                            session_id,
                            message,
                        });
                        running = false;
                    }
                    Ok(other) => {
                        let _ = evt_tx.send(PluginGuiEvent::Error {
                            session_id,
                            message: format!("gui poll: unexpected response: {other:?}"),
                        });
                        running = false;
                    }
                    Err(err) => {
                        let _ = evt_tx.send(PluginGuiEvent::Error {
                            session_id,
                            message: err,
                        });
                        running = false;
                    }
                }
            }
            client.close();
        });

        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.plugin_fx_draft.gui_status = crate::plugin::GuiSessionStatus::Opening;
            tab.plugin_fx_draft.last_error = None;
            tab.plugin_fx_draft.last_backend_log = Self::join_backend_log_lines(vec![
                "Native GUI: opening".to_string(),
                format!("Session: {session_id}"),
            ]);
        }

        self.plugin_gui_state = Some(PluginGuiSessionState {
            tab_path,
            session_id,
            started_at: Instant::now(),
            cmd_tx,
            rx: evt_rx,
        });
    }

    pub(super) fn sync_plugin_gui_for_tab(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let Some(state) = self.plugin_gui_state.as_ref() else {
            return;
        };
        if state.tab_path != tab.path {
            return;
        }
        let _ = state.cmd_tx.send(PluginGuiCommand::SyncNow);
    }

    pub(super) fn close_plugin_gui_for_tab(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let Some(state) = self.plugin_gui_state.as_ref() else {
            return;
        };
        if state.tab_path != tab.path {
            return;
        }
        let _ = state.cmd_tx.send(PluginGuiCommand::Close);
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.plugin_fx_draft.gui_status = crate::plugin::GuiSessionStatus::Closed;
        }
        self.plugin_gui_state = None;
    }

    fn spawn_plugin_process_for_tab(&mut self, tab_idx: usize, is_apply: bool) {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let Some(plugin_key) = tab.plugin_fx_draft.plugin_key.clone() else {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                tab.plugin_fx_draft.last_error = Some("plugin not selected".to_string());
            }
            return;
        };
        let plugin_path = self.plugin_key_to_path(&plugin_key);
        if !plugin_path.exists() {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                tab.plugin_fx_draft.last_error =
                    Some(format!("plugin not found: {}", plugin_path.display()));
            }
            return;
        }
        let out_sr = self.audio.shared.out_sample_rate.max(1);
        let channels = tab.ch_samples.clone();
        if channels.is_empty() {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                tab.plugin_fx_draft.last_error = Some("editor audio is empty".to_string());
            }
            return;
        }
        let draft = tab.plugin_fx_draft.clone();
        let undo = if is_apply {
            self.tabs.get(tab_idx).map(Self::capture_undo_state)
        } else {
            None
        };
        if self.plugin_process_state.is_some() {
            self.debug.plugin_stale_drop_count =
                self.debug.plugin_stale_drop_count.saturating_add(1);
            self.plugin_process_state = None;
        }
        let job_id = self.plugin_next_job_id();
        let (input_path, output_path) = match self.plugin_temp_paths(job_id) {
            Ok(v) => v,
            Err(err) => {
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.plugin_fx_draft.last_error = Some(err);
                }
                return;
            }
        };
        if let Err(err) = crate::wave::export_channels_audio_with_depth(
            &channels,
            out_sr,
            &input_path,
            Some(crate::wave::WavBitDepth::Float32),
        ) {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                tab.plugin_fx_draft.last_error = Some(format!("plugin input export failed: {err}"));
            }
            let _ = std::fs::remove_file(&input_path);
            let _ = std::fs::remove_file(&output_path);
            return;
        }
        let params: Vec<PluginParamValue> = draft
            .params
            .iter()
            .map(|p| PluginParamValue {
                id: p.id.clone(),
                normalized: p.normalized.clamp(0.0, 1.0),
            })
            .collect();
        let plugin_path_text = plugin_path.to_string_lossy().to_string();
        let input_path_text = input_path.to_string_lossy().to_string();
        let output_path_text = output_path.to_string_lossy().to_string();
        let (tx, rx) = mpsc::channel::<PluginProcessResult>();
        std::thread::spawn(move || {
            let req = WorkerRequest::ProcessFx {
                plugin_path: plugin_path_text,
                input_audio_path: input_path_text,
                output_audio_path: output_path_text.clone(),
                sample_rate: out_sr,
                max_block_size: 1024,
                enabled: draft.enabled,
                bypass: draft.bypass,
                state_blob_b64: draft
                    .state_blob
                    .as_ref()
                    .map(|bytes| base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes)),
                params,
            };
            let mut error: Option<String> = None;
            let mut channels_out: Vec<Vec<f32>> = Vec::new();
            let mut state_blob_out: Option<Vec<u8>> = None;
            let mut backend = crate::plugin::PluginHostBackend::Generic;
            let mut backend_note: Option<String> = None;
            match crate::plugin::client::run_request(&req) {
                Ok(WorkerResponse::ProcessResult {
                    output_audio_path,
                    state_blob_b64,
                    backend: b,
                    backend_note: note,
                }) => {
                    backend = b;
                    backend_note = note;
                    let out_path = PathBuf::from(output_audio_path);
                    match crate::audio_io::decode_audio_multi(&out_path) {
                        Ok((channels, _sr)) => {
                            channels_out = channels;
                            state_blob_out = state_blob_b64.and_then(|raw| {
                                base64::engine::general_purpose::STANDARD_NO_PAD
                                    .decode(raw.as_bytes())
                                    .ok()
                            });
                        }
                        Err(err) => {
                            error = Some(format!("plugin output decode failed: {err}"));
                        }
                    }
                }
                Ok(WorkerResponse::Error { message }) => {
                    error = Some(message);
                }
                Ok(_) => {
                    error = Some("plugin process: unexpected worker response".to_string());
                }
                Err(err) => {
                    error = Some(err);
                }
            }
            let _ = std::fs::remove_file(PathBuf::from(&output_path_text));
            let _ = std::fs::remove_file(input_path);
            let _ = tx.send(PluginProcessResult {
                job_id,
                tab_idx,
                is_apply,
                channels: channels_out,
                state_blob: state_blob_out,
                backend,
                backend_note,
                error,
            });
        });
        self.plugin_process_state = Some(PluginProcessState {
            job_id,
            started_at: Instant::now(),
            tab_idx,
            is_apply,
            rx,
            undo,
        });
    }

    pub(super) fn spawn_plugin_preview_for_tab(&mut self, tab_idx: usize) {
        self.spawn_plugin_process_for_tab(tab_idx, false);
    }

    pub(super) fn spawn_plugin_apply_for_tab(&mut self, tab_idx: usize) {
        self.spawn_plugin_process_for_tab(tab_idx, true);
    }

    pub(super) fn cancel_plugin_process(&mut self) {
        if self.plugin_process_state.is_some() {
            self.debug.plugin_stale_drop_count =
                self.debug.plugin_stale_drop_count.saturating_add(1);
        }
        self.plugin_process_state = None;
    }

    fn apply_gui_param_delta(tab: &mut crate::app::types::EditorTab, values: &[PluginParamValue]) -> usize {
        let mut applied = 0usize;
        for value in values.iter().take(64) {
            if let Some(param) = tab
                .plugin_fx_draft
                .params
                .iter_mut()
                .find(|p| p.id == value.id)
            {
                param.normalized = value.normalized.clamp(0.0, 1.0);
                applied += 1;
            }
        }
        applied
    }

    fn drain_plugin_gui_events(&mut self, ctx: &egui::Context) {
        let Some(gui_state) = self.plugin_gui_state.take() else {
            return;
        };
        let mut keep_state = true;
        let mut repaint = false;
        let tab_idx_opt = self.tabs.iter().position(|t| t.path == gui_state.tab_path);
        if tab_idx_opt.is_none() {
            let _ = gui_state.cmd_tx.send(PluginGuiCommand::Close);
            return;
        }
        let tab_idx = tab_idx_opt.unwrap_or(0);
        while let Ok(event) = gui_state.rx.try_recv() {
            repaint = true;
            match event {
                PluginGuiEvent::Opened {
                    session_id,
                    backend,
                    capabilities,
                    params,
                    state_blob,
                    backend_note,
                } => {
                    if session_id != gui_state.session_id {
                        self.debug.plugin_stale_drop_count =
                            self.debug.plugin_stale_drop_count.saturating_add(1);
                        continue;
                    }
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.plugin_fx_draft.backend = Some(backend);
                        tab.plugin_fx_draft.gui_capabilities = capabilities;
                        tab.plugin_fx_draft.gui_status = crate::plugin::GuiSessionStatus::Live;
                        if !params.is_empty() {
                            tab.plugin_fx_draft.params = params;
                        }
                        if state_blob.is_some() {
                            tab.plugin_fx_draft.state_blob = state_blob;
                        }
                        tab.plugin_fx_draft.last_error = None;
                        let mut lines = vec![format!(
                            "Native GUI opened: {:?} ({:.1} ms)",
                            backend,
                            gui_state.started_at.elapsed().as_secs_f32() * 1000.0
                        )];
                        if let Some(note) = backend_note {
                            lines.push(format!("Backend note: {}", note.trim()));
                        }
                        tab.plugin_fx_draft.last_backend_log = Self::join_backend_log_lines(lines);
                    }
                }
                PluginGuiEvent::Snapshot {
                    session_id,
                    params,
                    state_blob,
                    backend,
                    closed,
                    backend_note,
                } => {
                    if session_id != gui_state.session_id {
                        self.debug.plugin_stale_drop_count =
                            self.debug.plugin_stale_drop_count.saturating_add(1);
                        continue;
                    }
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        let applied = Self::apply_gui_param_delta(tab, &params);
                        if state_blob.is_some() {
                            tab.plugin_fx_draft.state_blob = state_blob;
                        }
                        tab.plugin_fx_draft.backend = Some(backend);
                        tab.plugin_fx_draft.last_error = None;
                        tab.plugin_fx_draft.gui_status = if closed {
                            crate::plugin::GuiSessionStatus::Closed
                        } else {
                            crate::plugin::GuiSessionStatus::Live
                        };
                        let mut lines = vec![format!(
                            "Native GUI sync: {:?}, params_applied={}",
                            backend, applied
                        )];
                        if let Some(note) = backend_note {
                            lines.push(format!("Backend note: {}", note.trim()));
                        }
                        tab.plugin_fx_draft.last_backend_log = Self::join_backend_log_lines(lines);
                    }
                    if closed {
                        keep_state = false;
                    }
                }
                PluginGuiEvent::Closed {
                    session_id,
                    state_blob,
                    backend,
                    backend_note,
                } => {
                    if session_id != gui_state.session_id {
                        self.debug.plugin_stale_drop_count =
                            self.debug.plugin_stale_drop_count.saturating_add(1);
                        continue;
                    }
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        if state_blob.is_some() {
                            tab.plugin_fx_draft.state_blob = state_blob;
                        }
                        tab.plugin_fx_draft.backend = Some(backend);
                        tab.plugin_fx_draft.gui_status = crate::plugin::GuiSessionStatus::Closed;
                        tab.plugin_fx_draft.last_error = None;
                        let mut lines = vec![format!("Native GUI closed: {:?}", backend)];
                        if let Some(note) = backend_note {
                            lines.push(format!("Backend note: {}", note.trim()));
                        }
                        tab.plugin_fx_draft.last_backend_log = Self::join_backend_log_lines(lines);
                    }
                    keep_state = false;
                }
                PluginGuiEvent::Error { session_id, message } => {
                    if session_id != gui_state.session_id {
                        self.debug.plugin_stale_drop_count =
                            self.debug.plugin_stale_drop_count.saturating_add(1);
                        continue;
                    }
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.plugin_fx_draft.gui_status = crate::plugin::GuiSessionStatus::Error;
                        tab.plugin_fx_draft.last_error = Some(message.clone());
                        tab.plugin_fx_draft.last_backend_log = Self::join_backend_log_lines(vec![
                            "Native GUI error".to_string(),
                            message,
                        ]);
                    }
                    keep_state = false;
                }
            }
        }
        if keep_state {
            self.plugin_gui_state = Some(gui_state);
        }
        if repaint {
            ctx.request_repaint();
        }
    }

    pub(super) fn drain_plugin_jobs(&mut self, ctx: &egui::Context) {
        self.debug.plugin_worker_timeout_count = crate::plugin::client::worker_timeout_count();

        if let Some(state) = self.plugin_scan_state.take() {
            match state.rx.try_recv() {
                Ok(result) => {
                    if result.job_id != state.job_id {
                        self.debug.plugin_stale_drop_count =
                            self.debug.plugin_stale_drop_count.saturating_add(1);
                    } else if let Some(err) = result.error {
                        self.debug_log(format!("plugin scan error: {err}"));
                        self.plugin_scan_error = Some(err);
                    } else {
                        self.plugin_catalog = result.plugins;
                        self.plugin_scan_error = None;
                    }
                    let elapsed_ms = state.started_at.elapsed().as_secs_f32() * 1000.0;
                    Self::plugin_push_metric(&mut self.debug.plugin_scan_ms, elapsed_ms);
                    ctx.request_repaint();
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.plugin_scan_state = Some(state);
                }
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
        }

        if let Some(state) = self.plugin_probe_state.take() {
            match state.rx.try_recv() {
                Ok(result) => {
                    let elapsed_ms = state.started_at.elapsed().as_secs_f32() * 1000.0;
                    Self::plugin_push_metric(&mut self.debug.plugin_probe_ms, elapsed_ms);
                    if result.job_id != state.job_id {
                        self.debug.plugin_stale_drop_count =
                            self.debug.plugin_stale_drop_count.saturating_add(1);
                    } else if let Some(tab_idx) =
                        self.tabs.iter().position(|t| t.path == state.tab_path)
                    {
                        if let Some(tab) = self.tabs.get_mut(tab_idx) {
                            if let Some(err) = result.error {
                                tab.plugin_fx_draft.last_backend_log =
                                    Self::join_backend_log_lines(vec![
                                        format!("Probe failed in {:.1} ms", elapsed_ms),
                                        err.clone(),
                                    ]);
                                tab.plugin_fx_draft.last_error = Some(err);
                            } else {
                                let fallback_hint = if result.backend
                                    == crate::plugin::PluginHostBackend::Generic
                                    && Self::is_native_plugin_key(&result.plugin_key)
                                {
                                    Self::native_probe_fallback_hint(
                                        &result.plugin_key,
                                        result.params.is_empty(),
                                    )
                                } else {
                                    None
                                };
                                if result.backend == crate::plugin::PluginHostBackend::Generic
                                    && Self::is_native_plugin_key(&result.plugin_key)
                                {
                                    self.debug.plugin_native_fallback_count = self
                                        .debug
                                        .plugin_native_fallback_count
                                        .saturating_add(1);
                                }
                                tab.plugin_fx_draft.plugin_key = Some(result.plugin_key);
                                tab.plugin_fx_draft.plugin_name = result.plugin_name;
                                tab.plugin_fx_draft.params = result.params;
                                tab.plugin_fx_draft.state_blob = result.state_blob;
                                tab.plugin_fx_draft.backend = Some(result.backend);
                                tab.plugin_fx_draft.gui_capabilities = result.capabilities;
                                tab.plugin_fx_draft.gui_status =
                                    crate::plugin::GuiSessionStatus::Closed;
                                tab.plugin_fx_draft.enabled = true;
                                tab.plugin_fx_draft.bypass = false;
                                tab.plugin_fx_draft.last_error = fallback_hint;
                                let mut lines = vec![format!(
                                    "Probe: {:?}, params={}, {:.1} ms",
                                    result.backend,
                                    tab.plugin_fx_draft.params.len(),
                                    elapsed_ms
                                )];
                                if let Some(note) = result.backend_note.as_deref() {
                                    lines.push(format!("Backend note: {}", note.trim()));
                                }
                                tab.plugin_fx_draft.last_backend_log =
                                    Self::join_backend_log_lines(lines);
                            }
                        }
                    }
                    ctx.request_repaint();
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.plugin_probe_state = Some(state);
                }
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
        }

        if let Some(mut state) = self.plugin_process_state.take() {
            match state.rx.try_recv() {
                Ok(result) => {
                    if result.job_id != state.job_id
                        || result.tab_idx != state.tab_idx
                        || result.is_apply != state.is_apply
                    {
                        self.debug.plugin_stale_drop_count =
                            self.debug.plugin_stale_drop_count.saturating_add(1);
                        ctx.request_repaint();
                        return;
                    }
                    let elapsed_ms = state.started_at.elapsed().as_secs_f32() * 1000.0;
                    if let Some(err) = result.error {
                        if let Some(tab) = self.tabs.get_mut(result.tab_idx) {
                            let phase = if result.is_apply { "Apply" } else { "Preview" };
                            tab.plugin_fx_draft.last_backend_log =
                                Self::join_backend_log_lines(vec![
                                    format!("{phase} failed in {:.1} ms", elapsed_ms),
                                    err.clone(),
                                    "If this is channel-related, check plugin bus layout support."
                                        .to_string(),
                                ]);
                            tab.plugin_fx_draft.last_error = Some(err);
                        }
                        ctx.request_repaint();
                        return;
                    }
                    if result.channels.is_empty() {
                        if let Some(tab) = self.tabs.get_mut(result.tab_idx) {
                            let phase = if result.is_apply { "Apply" } else { "Preview" };
                            tab.plugin_fx_draft.last_backend_log =
                                Self::join_backend_log_lines(vec![
                                    format!("{phase} returned empty audio in {:.1} ms", elapsed_ms),
                                    "Possible unsupported channel/bus layout or plugin failure."
                                        .to_string(),
                                ]);
                            tab.plugin_fx_draft.last_error =
                                Some("plugin process returned empty audio".to_string());
                        }
                        ctx.request_repaint();
                        return;
                    }
                    if result.is_apply {
                        let editor_channels_before = self
                            .tabs
                            .get(result.tab_idx)
                            .map(|t| t.ch_samples.len())
                            .unwrap_or(0);
                        let native_requested = self
                            .tabs
                            .get(result.tab_idx)
                            .and_then(|t| t.plugin_fx_draft.plugin_key.as_deref())
                            .map(Self::is_native_plugin_key)
                            .unwrap_or(false);
                        if native_requested
                            && result.backend == crate::plugin::PluginHostBackend::Generic
                        {
                            self.debug.plugin_native_fallback_count = self
                                .debug
                                .plugin_native_fallback_count
                                .saturating_add(1);
                        }
                        if let Some(tab) = self.tabs.get_mut(result.tab_idx) {
                            if let Some(undo) = state.undo.take() {
                                Self::push_undo_state_from(tab, undo, true);
                            }
                            tab.preview_audio_tool = None;
                            tab.preview_overlay = None;
                            tab.ch_samples = result.channels;
                            tab.samples_len = tab.ch_samples.get(0).map(|c| c.len()).unwrap_or(0);
                            tab.dirty = true;
                            Self::editor_clamp_ranges(tab);
                            tab.plugin_fx_draft.state_blob = result.state_blob;
                            tab.plugin_fx_draft.backend = Some(result.backend);
                            tab.plugin_fx_draft.last_error = None;
                            let processed_channels = tab.ch_samples.len();
                            let frames = tab
                                .ch_samples
                                .get(0)
                                .map(|c| c.len())
                                .unwrap_or(0);
                            let mut lines = vec![format!(
                                "Apply: {:?}, {}ch {} smp, {:.1} ms",
                                result.backend,
                                processed_channels,
                                frames,
                                elapsed_ms
                            )];
                            lines.push("Apply audio source: processed channels (committed)".to_string());
                            lines.push(format!(
                                "Channels: processed={} / editor_before={}",
                                processed_channels, editor_channels_before
                            ));
                            if editor_channels_before > 2 {
                                lines.push(
                                    "Input has >2 channels; host bridge is optimized for mono/stereo."
                                        .to_string(),
                                );
                            }
                            if processed_channels != editor_channels_before {
                                lines.push(
                                    "Channel mismatch: plugin/backend may not support the original layout."
                                        .to_string(),
                                );
                            }
                            if let Some(note) = result.backend_note.as_deref() {
                                lines.push(format!("Backend note: {}", note.trim()));
                            }
                            tab.plugin_fx_draft.last_backend_log =
                                Self::join_backend_log_lines(lines);
                        }
                        if let Some(tab) = self.tabs.get(result.tab_idx) {
                            self.audio.stop();
                            self.audio.set_samples_channels(tab.ch_samples.clone());
                            self.apply_loop_mode_for_tab(tab);
                            let len = tab.samples_len;
                            let clamped_pos = if len == 0 {
                                0usize
                            } else {
                                let pos = self
                                    .audio
                                    .shared
                                    .play_pos
                                    .load(std::sync::atomic::Ordering::Relaxed);
                                pos.min(len.saturating_sub(1))
                            };
                            self.audio
                                .shared
                                .play_pos
                                .store(clamped_pos, std::sync::atomic::Ordering::Relaxed);
                            self.audio
                                .shared
                                .play_pos_f
                                .store(clamped_pos as f32, std::sync::atomic::Ordering::Relaxed);
                        }
                        Self::plugin_push_metric(&mut self.debug.plugin_apply_ms, elapsed_ms);
                    } else {
                        let native_requested = self
                            .tabs
                            .get(result.tab_idx)
                            .and_then(|t| t.plugin_fx_draft.plugin_key.as_deref())
                            .map(Self::is_native_plugin_key)
                            .unwrap_or(false);
                        if native_requested
                            && result.backend == crate::plugin::PluginHostBackend::Generic
                        {
                            self.debug.plugin_native_fallback_count = self
                                .debug
                                .plugin_native_fallback_count
                                .saturating_add(1);
                        }
                        let preview_channels = result.channels;
                        let preview_ch_count = preview_channels.len();
                        let timeline_len =
                            preview_channels.get(0).map(|c| c.len()).unwrap_or(0);
                        let overlay = Self::preview_overlay_from_channels(
                            preview_channels.clone(),
                            ToolKind::PluginFx,
                            timeline_len,
                        );
                        if let Some(tab) = self.tabs.get_mut(result.tab_idx) {
                            let editor_ch_count = tab.ch_samples.len();
                            tab.preview_overlay = Some(overlay);
                            tab.preview_audio_tool = Some(ToolKind::PluginFx);
                            tab.plugin_fx_draft.state_blob = result.state_blob;
                            tab.plugin_fx_draft.backend = Some(result.backend);
                            tab.plugin_fx_draft.last_error = None;
                            let frames = tab
                                .preview_overlay
                                .as_ref()
                                .map(|o| o.timeline_len)
                                .unwrap_or(0);
                            let mut lines = vec![format!(
                                "Preview: {:?}, {}ch {} smp, {:.1} ms",
                                result.backend,
                                preview_ch_count,
                                frames,
                                elapsed_ms
                            )];
                            lines.push("Preview audio source: processed channels".to_string());
                            lines.push(format!(
                                "Channels: processed={} / editor={}",
                                preview_ch_count, editor_ch_count
                            ));
                            if editor_ch_count > 2 {
                                lines.push(
                                    "Input has >2 channels; host bridge is optimized for mono/stereo."
                                        .to_string(),
                                );
                            }
                            if preview_ch_count != editor_ch_count {
                                lines.push(
                                    "Channel mismatch: plugin/backend may not support the original layout."
                                        .to_string(),
                                );
                            }
                            if let Some(note) = result.backend_note.as_deref() {
                                lines.push(format!("Backend note: {}", note.trim()));
                            }
                            tab.plugin_fx_draft.last_backend_log =
                                Self::join_backend_log_lines(lines);
                        }
                        if preview_ch_count > 0 {
                            self.set_preview_channels(
                                result.tab_idx,
                                ToolKind::PluginFx,
                                preview_channels,
                            );
                        }
                        Self::plugin_push_metric(&mut self.debug.plugin_preview_ms, elapsed_ms);
                    }
                    ctx.request_repaint();
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.plugin_process_state = Some(state);
                }
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
        }
        self.drain_plugin_gui_events(ctx);
    }

    pub(super) fn plugin_path_label(path: &Path) -> String {
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("(plugin)")
            .to_string()
    }
}
