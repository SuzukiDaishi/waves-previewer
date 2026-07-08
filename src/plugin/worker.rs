use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::plugin::backends::{clap, generic, vst3};
use crate::plugin::protocol::{GuiCapabilities, PluginHostBackend, WorkerRequest, WorkerResponse};

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
                crate::plugin::PluginFormat::Vst3 => match probe_with_retry(|| vst3::probe(&path)) {
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
                        "native VST3 probe failed after {PROBE_RETRY_ATTEMPTS} attempts, fallback=Generic: {err}"
                    )),
                },
                crate::plugin::PluginFormat::Clap => match probe_with_retry(|| clap::probe(&path)) {
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
                        "native CLAP probe failed after {PROBE_RETRY_ATTEMPTS} attempts, fallback=Generic: {err}"
                    )),
                },
            };
            let (plugin, _params, state_blob_b64) = generic::default_probe_result(&path, format);
            WorkerResponse::ProbeResult {
                plugin,
                params: Vec::new(),
                state_blob_b64,
                backend: PluginHostBackend::Generic,
                capabilities: GuiCapabilities::default(),
                backend_note: native_error,
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
                            native_error = Some(format!(
                                "native VST3 process failed, fallback=Generic: {err}"
                            ));
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
                            native_error = Some(format!(
                                "native CLAP process failed, fallback=Generic: {err}"
                            ));
                        }
                    }
                }
                None => {}
            }

            match generic::process(&input, &output, enabled, bypass, &params) {
                Ok(()) => WorkerResponse::ProcessResult {
                    output_audio_path,
                    state_blob_b64: state_blob_b64.or_else(|| generic::state_blob_b64(&params)),
                    backend: PluginHostBackend::Generic,
                    backend_note: native_error,
                },
                Err(message) => WorkerResponse::Error {
                    message: match native_error {
                        Some(native) => format!("{native}; generic process failed: {message}"),
                        None => message,
                    },
                },
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
