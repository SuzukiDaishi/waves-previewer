use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use crate::plugin::backends::{clap, generic, vst3};
use crate::plugin::protocol::{
    GuiCapabilities, PluginChainSlotConfig, PluginHostBackend, WorkerRequest, WorkerResponse,
};

#[derive(Clone)]
struct ChainRuntime {
    slots: Vec<PluginChainSlotConfig>,
    sample_rate: u32,
    max_block_size: usize,
    render_ahead_ms: u32,
    bypass: bool,
    underruns: u64,
}

static CHAIN_SESSIONS: LazyLock<Mutex<HashMap<u64, ChainRuntime>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn process_chain_request(
    slots: Vec<PluginChainSlotConfig>,
    input_audio_path: String,
    output_audio_path: String,
    sample_rate: u32,
    max_block_size: usize,
    chain_bypass: bool,
) -> WorkerResponse {
    if chain_bypass || slots.iter().all(|slot| !slot.enabled || slot.bypass) {
        return match std::fs::copy(&input_audio_path, &output_audio_path) {
            Ok(_) => WorkerResponse::ChainProcessResult {
                output_audio_path,
                slot_state_blobs_b64: Vec::new(),
                latency_samples: 0,
                underruns: 0,
                failed_slot: None,
                backend_note: Some("chain bypassed; dry audio copied".to_string()),
            },
            Err(error) => WorkerResponse::Error {
                message: format!("chain bypass copy failed: {error}"),
            },
        };
    }
    let active: Vec<_> = slots
        .into_iter()
        .filter(|slot| slot.enabled && !slot.bypass)
        .collect();
    let mut current_input = PathBuf::from(&input_audio_path);
    let mut temporary = Vec::new();
    let mut states = Vec::new();
    for (index, slot) in active.iter().enumerate() {
        let is_last = index + 1 == active.len();
        let output = if is_last {
            PathBuf::from(&output_audio_path)
        } else {
            let path = std::env::temp_dir().join(format!(
                "neowaves_chain_{}_{}_{}.wav",
                std::process::id(),
                slot.slot_id,
                index
            ));
            temporary.push(path.clone());
            path
        };
        let response = handle_request(WorkerRequest::ProcessFx {
            plugin_path: slot.plugin_path.clone(),
            input_audio_path: current_input.to_string_lossy().to_string(),
            output_audio_path: output.to_string_lossy().to_string(),
            sample_rate,
            max_block_size,
            enabled: true,
            bypass: false,
            state_blob_b64: slot.state_blob_b64.clone(),
            params: slot.params.clone(),
        });
        match response {
            WorkerResponse::ProcessResult {
                state_blob_b64,
                backend_note,
                ..
            } => {
                states.push((slot.slot_id, state_blob_b64));
                if backend_note.is_some() {
                    for path in temporary {
                        let _ = std::fs::remove_file(path);
                    }
                    return WorkerResponse::ChainProcessResult {
                        output_audio_path,
                        slot_state_blobs_b64: states,
                        latency_samples: 0,
                        underruns: 0,
                        failed_slot: Some(slot.slot_id),
                        backend_note,
                    };
                }
            }
            WorkerResponse::Error { message } => {
                for path in temporary {
                    let _ = std::fs::remove_file(path);
                }
                return WorkerResponse::Error {
                    message: format!("chain slot {} failed: {message}", slot.slot_id),
                };
            }
            other => {
                for path in temporary {
                    let _ = std::fs::remove_file(path);
                }
                return WorkerResponse::Error {
                    message: format!("chain slot {} unexpected response: {other:?}", slot.slot_id),
                };
            }
        }
        current_input = output;
    }
    for path in temporary {
        let _ = std::fs::remove_file(path);
    }
    WorkerResponse::ChainProcessResult {
        output_audio_path,
        slot_state_blobs_b64: states,
        latency_samples: 0,
        underruns: 0,
        failed_slot: None,
        backend_note: None,
    }
}

fn format_from_plugin_path(path: &str) -> Option<crate::plugin::PluginFormat> {
    generic::format_from_path(&PathBuf::from(path))
}

fn path_priority_insert(
    by_path: &mut BTreeMap<String, (u8, crate::plugin::PluginDescriptorInfo)>,
    priority: u8,
    desc: crate::plugin::PluginDescriptorInfo,
) {
    match by_path.get(&desc.path) {
        Some((current, _)) if *current > priority => {}
        _ => {
            by_path.insert(desc.path.clone(), (priority, desc));
        }
    }
}

fn plugin_is_allowed_for_fx(path: &std::path::Path, format: crate::plugin::PluginFormat) -> bool {
    match format {
        crate::plugin::PluginFormat::Vst3 => vst3::is_audio_effect_plugin(path).unwrap_or(true),
        crate::plugin::PluginFormat::Clap => clap::is_audio_effect_plugin(path).unwrap_or(true),
    }
}

/// Native probing launches the plugin in-process and is inherently racy
/// (module load / COM init / plugin init timing), so a single transient
/// failure would otherwise permanently downgrade a session to the
/// zero-param Generic backend. Retry a couple of times before giving up.
const PROBE_RETRY_ATTEMPTS: u32 = 3;
const PROBE_RETRY_DELAY_MS: u64 = 150;

type ProbeOk = (
    crate::plugin::PluginDescriptorInfo,
    Vec<crate::plugin::PluginParamInfo>,
    Option<String>,
);

fn probe_with_retry<F>(mut probe_fn: F) -> Result<ProbeOk, String>
where
    F: FnMut() -> Result<ProbeOk, String>,
{
    let mut last_err = String::new();
    for attempt in 0..PROBE_RETRY_ATTEMPTS {
        match probe_fn() {
            Ok(result) => return Ok(result),
            Err(err) => {
                last_err = err;
                if attempt + 1 < PROBE_RETRY_ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(PROBE_RETRY_DELAY_MS));
                }
            }
        }
    }
    Err(last_err)
}

pub fn handle_request(request: WorkerRequest) -> WorkerResponse {
    match request {
        WorkerRequest::Ping => WorkerResponse::Pong,
        WorkerRequest::Scan { search_paths } => {
            let mut by_path: BTreeMap<String, (u8, crate::plugin::PluginDescriptorInfo)> =
                BTreeMap::new();
            for desc in generic::scan_paths(&search_paths) {
                path_priority_insert(&mut by_path, 0, desc);
            }
            if let Ok(native) = vst3::scan_paths(&search_paths) {
                for desc in native {
                    path_priority_insert(&mut by_path, 1, desc);
                }
            }
            if let Ok(native) = clap::scan_paths(&search_paths) {
                for desc in native {
                    path_priority_insert(&mut by_path, 1, desc);
                }
            }
            let plugins = by_path
                .into_values()
                .map(|(_, desc)| desc)
                .filter(|desc| plugin_is_allowed_for_fx(&PathBuf::from(&desc.path), desc.format))
                .collect();
            WorkerResponse::ScanResult { plugins }
        }
        WorkerRequest::Probe { plugin_path } => {
            let path = PathBuf::from(&plugin_path);
            let Some(format) = format_from_plugin_path(&plugin_path) else {
                return WorkerResponse::Error {
                    message: format!("unsupported plugin format: {plugin_path}"),
                };
            };
            if !plugin_is_allowed_for_fx(&path, format) {
                return WorkerResponse::Error {
                    message: format!(
                        "instrument/synth plugins are excluded from Plugin FX ({})",
                        path.display()
                    ),
                };
            }
            let native_error = match format {
                crate::plugin::PluginFormat::Vst3 => {
                    match probe_with_retry(|| vst3::probe(&path)) {
                        Ok((plugin, params, state_blob_b64)) => {
                            return WorkerResponse::ProbeResult {
                                plugin,
                                params,
                                state_blob_b64,
                                backend: PluginHostBackend::NativeVst3,
                                capabilities: GuiCapabilities {
                                    supports_native_gui: cfg!(all(
                                        feature = "plugin_native_vst3",
                                        windows
                                    )),
                                    supports_param_feedback: cfg!(feature = "plugin_native_vst3"),
                                    supports_state_sync: cfg!(feature = "plugin_native_vst3"),
                                },
                                backend_note: None,
                            };
                        }
                        Err(err) => Some(format!(
                            "native VST3 probe failed after {PROBE_RETRY_ATTEMPTS} attempts: {err}"
                        )),
                    }
                }
                crate::plugin::PluginFormat::Clap => {
                    match probe_with_retry(|| clap::probe(&path)) {
                        Ok((plugin, params, state_blob_b64)) => {
                            return WorkerResponse::ProbeResult {
                                plugin,
                                params,
                                state_blob_b64,
                                backend: PluginHostBackend::NativeClap,
                                capabilities: GuiCapabilities {
                                    supports_native_gui: cfg!(all(
                                        feature = "plugin_native_clap",
                                        windows
                                    )),
                                    supports_param_feedback: cfg!(feature = "plugin_native_clap"),
                                    supports_state_sync: cfg!(feature = "plugin_native_clap"),
                                },
                                backend_note: None,
                            };
                        }
                        Err(err) => Some(format!(
                            "native CLAP probe failed after {PROBE_RETRY_ATTEMPTS} attempts: {err}"
                        )),
                    }
                }
            };
            WorkerResponse::Error {
                message: native_error
                    .unwrap_or_else(|| format!("native plugin probe failed: {}", path.display())),
            }
        }
        WorkerRequest::ProcessFx {
            plugin_path,
            input_audio_path,
            output_audio_path,
            sample_rate,
            max_block_size,
            enabled,
            bypass,
            state_blob_b64,
            params,
        } => {
            let plugin_path_buf = PathBuf::from(&plugin_path);
            let input = PathBuf::from(&input_audio_path);
            let output = PathBuf::from(&output_audio_path);
            let mut native_error: Option<String> = None;

            match format_from_plugin_path(&plugin_path) {
                Some(format @ crate::plugin::PluginFormat::Vst3) => {
                    if !plugin_is_allowed_for_fx(&plugin_path_buf, format) {
                        return WorkerResponse::Error {
                            message: format!(
                                "instrument/synth plugins are excluded from Plugin FX ({})",
                                plugin_path_buf.display()
                            ),
                        };
                    }
                    match vst3::process(
                        &plugin_path_buf,
                        &input,
                        &output,
                        sample_rate,
                        max_block_size,
                        enabled,
                        bypass,
                        state_blob_b64.as_deref(),
                        &params,
                    ) {
                        Ok(state_blob_b64_out) => {
                            return WorkerResponse::ProcessResult {
                                output_audio_path,
                                state_blob_b64: state_blob_b64_out,
                                backend: PluginHostBackend::NativeVst3,
                                backend_note: None,
                            };
                        }
                        Err(err) => {
                            native_error = Some(format!("native VST3 process failed: {err}"));
                        }
                    }
                }
                Some(format @ crate::plugin::PluginFormat::Clap) => {
                    if !plugin_is_allowed_for_fx(&plugin_path_buf, format) {
                        return WorkerResponse::Error {
                            message: format!(
                                "instrument/synth plugins are excluded from Plugin FX ({})",
                                plugin_path_buf.display()
                            ),
                        };
                    }
                    match clap::process(
                        &plugin_path_buf,
                        &input,
                        &output,
                        sample_rate,
                        max_block_size,
                        enabled,
                        bypass,
                        state_blob_b64.as_deref(),
                        &params,
                    ) {
                        Ok(state_blob_b64_out) => {
                            return WorkerResponse::ProcessResult {
                                output_audio_path,
                                state_blob_b64: state_blob_b64_out,
                                backend: PluginHostBackend::NativeClap,
                                backend_note: None,
                            };
                        }
                        Err(err) => {
                            native_error = Some(format!("native CLAP process failed: {err}"));
                        }
                    }
                }
                None => {}
            }

            WorkerResponse::Error {
                message: native_error.unwrap_or_else(|| {
                    format!(
                        "unsupported or unavailable native plugin: {}",
                        plugin_path_buf.display()
                    )
                }),
            }
        }
        WorkerRequest::ProcessChain {
            slots,
            input_audio_path,
            output_audio_path,
            sample_rate,
            max_block_size,
            chain_bypass,
        } => process_chain_request(
            slots,
            input_audio_path,
            output_audio_path,
            sample_rate,
            max_block_size,
            chain_bypass,
        ),
        WorkerRequest::ChainSessionOpen {
            session_id,
            slots,
            sample_rate,
            channels: _,
            max_block_size,
            render_ahead_ms,
        } => {
            let render_ahead_ms = render_ahead_ms.clamp(100, 500);
            CHAIN_SESSIONS.lock().unwrap().insert(
                session_id,
                ChainRuntime {
                    slots,
                    sample_rate: sample_rate.max(1),
                    max_block_size: max_block_size.max(1),
                    render_ahead_ms,
                    bypass: false,
                    underruns: 0,
                },
            );
            WorkerResponse::ChainSessionOpened {
                session_id,
                render_ahead_ms,
            }
        }
        WorkerRequest::ChainSessionConfigure {
            session_id,
            slots,
            chain_bypass,
        } => {
            let mut sessions = CHAIN_SESSIONS.lock().unwrap();
            let Some(runtime) = sessions.get_mut(&session_id) else {
                return WorkerResponse::Error {
                    message: format!("plugin chain session not found: {session_id}"),
                };
            };
            runtime.slots = slots;
            runtime.bypass = chain_bypass;
            WorkerResponse::ChainSessionAck {
                session_id,
                latency_samples: 0,
                underruns: runtime.underruns,
            }
        }
        WorkerRequest::ChainSessionProcess {
            session_id,
            input_audio_path,
            output_audio_path,
        } => {
            let runtime = CHAIN_SESSIONS.lock().unwrap().get(&session_id).cloned();
            let Some(runtime) = runtime else {
                return WorkerResponse::Error {
                    message: format!("plugin chain session not found: {session_id}"),
                };
            };
            let mut response = process_chain_request(
                runtime.slots,
                input_audio_path,
                output_audio_path,
                runtime.sample_rate,
                runtime.max_block_size,
                runtime.bypass,
            );
            if let WorkerResponse::ChainProcessResult { backend_note, .. } = &mut response {
                if backend_note.is_none() {
                    *backend_note = Some(format!(
                        "isolated render-ahead session ({} ms)",
                        runtime.render_ahead_ms
                    ));
                }
            }
            response
        }
        WorkerRequest::ChainSessionSeek { session_id, .. }
        | WorkerRequest::ChainSessionFlush { session_id } => {
            let sessions = CHAIN_SESSIONS.lock().unwrap();
            let Some(runtime) = sessions.get(&session_id) else {
                return WorkerResponse::Error {
                    message: format!("plugin chain session not found: {session_id}"),
                };
            };
            WorkerResponse::ChainSessionAck {
                session_id,
                latency_samples: 0,
                underruns: runtime.underruns,
            }
        }
        WorkerRequest::ChainSessionClose { session_id } => {
            CHAIN_SESSIONS.lock().unwrap().remove(&session_id);
            WorkerResponse::ChainSessionAck {
                session_id,
                latency_samples: 0,
                underruns: 0,
            }
        }
        WorkerRequest::GuiSessionOpen { .. }
        | WorkerRequest::GuiSessionPoll { .. }
        | WorkerRequest::GuiSessionClose { .. }
        | WorkerRequest::Heartbeat { .. } => WorkerResponse::Error {
            message: "GUI session requests require neowaves_plugin_gui_worker".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn probe_ok() -> ProbeOk {
        (
            crate::plugin::PluginDescriptorInfo {
                key: "k".to_string(),
                name: "n".to_string(),
                path: "p".to_string(),
                format: crate::plugin::PluginFormat::Vst3,
            },
            Vec::new(),
            None,
        )
    }

    #[test]
    fn probe_with_retry_succeeds_after_transient_failures() {
        let attempts = AtomicU32::new(0);
        let result = probe_with_retry(|| {
            let n = attempts.fetch_add(1, Ordering::SeqCst);
            if n < PROBE_RETRY_ATTEMPTS - 1 {
                Err("transient".to_string())
            } else {
                Ok(probe_ok())
            }
        });
        assert!(result.is_ok());
        assert_eq!(attempts.load(Ordering::SeqCst), PROBE_RETRY_ATTEMPTS);
    }

    #[test]
    fn probe_with_retry_gives_up_after_max_attempts() {
        let attempts = AtomicU32::new(0);
        let result = probe_with_retry(|| {
            attempts.fetch_add(1, Ordering::SeqCst);
            Err::<ProbeOk, String>("permanent".to_string())
        });
        assert_eq!(result.unwrap_err(), "permanent");
        assert_eq!(attempts.load(Ordering::SeqCst), PROBE_RETRY_ATTEMPTS);
    }

    #[test]
    fn probe_with_retry_succeeds_first_try_without_retrying() {
        let attempts = AtomicU32::new(0);
        let result = probe_with_retry(|| {
            attempts.fetch_add(1, Ordering::SeqCst);
            Ok(probe_ok())
        });
        assert!(result.is_ok());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
}
