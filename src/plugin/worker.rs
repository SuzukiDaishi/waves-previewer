use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::plugin::backends::{clap, generic, vst3};
use crate::plugin::protocol::{
    PluginHostBackend, WorkerRequest, WorkerResponse,
};

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
                    match vst3::probe(&path) {
                        Ok((plugin, params, state_blob_b64)) => {
                            return WorkerResponse::ProbeResult {
                                plugin,
                                params,
                                state_blob_b64,
                                backend: PluginHostBackend::NativeVst3,
                                backend_note: None,
                            };
                        }
                        Err(err) => {
                            Some(format!("native VST3 probe failed, fallback=Generic: {err}"))
                        }
                    }
                }
                crate::plugin::PluginFormat::Clap => {
                    match clap::probe(&path) {
                        Ok((plugin, params, state_blob_b64)) => {
                            return WorkerResponse::ProbeResult {
                                plugin,
                                params,
                                state_blob_b64,
                                backend: PluginHostBackend::NativeClap,
                                backend_note: None,
                            };
                        }
                        Err(err) => {
                            Some(format!("native CLAP probe failed, fallback=Generic: {err}"))
                        }
                    }
                }
            };
            let (plugin, _params, state_blob_b64) = generic::default_probe_result(&path, format);
            WorkerResponse::ProbeResult {
                plugin,
                params: Vec::new(),
                state_blob_b64,
                backend: PluginHostBackend::Generic,
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
    }
}
