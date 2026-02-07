use std::path::Path;

use crate::plugin::protocol::{PluginDescriptorInfo, PluginParamInfo, PluginParamValue};

#[cfg(feature = "plugin_native_clap")]
mod native {
    use super::*;
    use std::ffi::OsStr;

    use clack_host::prelude::*;
    use crate::plugin::backends::{default_params, plugin_display_name};
    use crate::plugin::PluginFormat;

    fn candidate_bundles(search_paths: &[String]) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        for raw in search_paths {
            let root = Path::new(raw);
            if !root.exists() {
                continue;
            }
            if root.is_file()
                && root
                    .extension()
                    .and_then(OsStr::to_str)
                    .map(|ext| ext.eq_ignore_ascii_case("clap"))
                    .unwrap_or(false)
            {
                out.push(root.to_path_buf());
                continue;
            }
            for entry in walkdir::WalkDir::new(root)
                .follow_links(false)
                .max_depth(8)
                .into_iter()
                .filter_map(Result::ok)
            {
                let p = entry.path();
                if !p.is_file() {
                    continue;
                }
                if p.extension()
                    .and_then(OsStr::to_str)
                    .map(|ext| ext.eq_ignore_ascii_case("clap"))
                    .unwrap_or(false)
                {
                    out.push(p.to_path_buf());
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    fn first_descriptor_name_and_id(plugin_path: &Path) -> Result<(String, String), String> {
        let bundle = unsafe { PluginBundle::load(plugin_path) }
            .map_err(|e| format!("clap load failed: {e}"))?;
        let factory = bundle
            .get_plugin_factory()
            .ok_or_else(|| "clap missing plugin factory".to_string())?;
        let descriptor = factory
            .plugin_descriptors()
            .next()
            .ok_or_else(|| "clap has no plugin descriptor".to_string())?;
        let plugin_id = descriptor
            .id()
            .and_then(|id| id.to_str().ok())
            .ok_or_else(|| "clap descriptor id missing".to_string())?
            .to_string();
        let plugin_name = descriptor
            .name()
            .map(|v| v.to_string_lossy().to_string())
            .unwrap_or_else(|| plugin_display_name(plugin_path));
        Ok((plugin_name, plugin_id))
    }

    fn first_descriptor_features(plugin_path: &Path) -> Result<Vec<String>, String> {
        let bundle = unsafe { PluginBundle::load(plugin_path) }
            .map_err(|e| format!("clap load failed: {e}"))?;
        let factory = bundle
            .get_plugin_factory()
            .ok_or_else(|| "clap missing plugin factory".to_string())?;
        let descriptor = factory
            .plugin_descriptors()
            .next()
            .ok_or_else(|| "clap has no plugin descriptor".to_string())?;
        Ok(descriptor
            .features()
            .map(|f| f.to_string_lossy().to_ascii_lowercase())
            .collect())
    }

    pub(super) fn has_synth_like_feature(features: &[String]) -> bool {
        features.iter().any(|f| {
            matches!(
                f.as_str(),
                "instrument" | "synthesizer" | "sampler" | "drum" | "drum-machine"
            )
        })
    }

    pub(crate) fn is_audio_effect_plugin(plugin_path: &Path) -> Result<bool, String> {
        let features = first_descriptor_features(plugin_path)?;
        if has_synth_like_feature(&features) {
            return Ok(false);
        }
        if features.is_empty() {
            return Ok(true);
        }
        Ok(features.iter().any(|f| f == "audio-effect"))
    }

    pub(crate) fn scan_paths(search_paths: &[String]) -> Result<Vec<PluginDescriptorInfo>, String> {
        let mut out = Vec::new();
        for plugin_path in candidate_bundles(search_paths) {
            if !is_audio_effect_plugin(&plugin_path).unwrap_or(true) {
                continue;
            }
            let Ok((name, _id)) = first_descriptor_name_and_id(&plugin_path) else {
                continue;
            };
            let path_str = plugin_path.to_string_lossy().to_string();
            out.push(PluginDescriptorInfo {
                key: path_str.clone(),
                name,
                path: path_str,
                format: PluginFormat::Clap,
            });
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out.dedup_by(|a, b| a.path == b.path);
        Ok(out)
    }

    pub(crate) fn probe(
        plugin_path: &Path,
    ) -> Result<(PluginDescriptorInfo, Vec<PluginParamInfo>, Option<String>), String> {
        if !is_audio_effect_plugin(plugin_path).unwrap_or(true) {
            return Err(format!(
                "instrument/synth plugins are excluded from Plugin FX ({})",
                plugin_path.display()
            ));
        }
        let (name, _id) = first_descriptor_name_and_id(plugin_path)?;
        let path_str = plugin_path.to_string_lossy().to_string();
        Ok((
            PluginDescriptorInfo {
                key: path_str.clone(),
                name,
                path: path_str,
                format: PluginFormat::Clap,
            },
            default_params(),
            None,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process(
        plugin_path: &Path,
        input_audio_path: &Path,
        output_audio_path: &Path,
        sample_rate: u32,
        max_block_size: usize,
        enabled: bool,
        bypass: bool,
        state_blob_b64: Option<&str>,
        _params: &[PluginParamValue],
    ) -> Result<Option<String>, String> {
        if !is_audio_effect_plugin(plugin_path).unwrap_or(true) {
            return Err(format!(
                "instrument/synth plugins are excluded from Plugin FX ({})",
                plugin_path.display()
            ));
        }
        let (channels, input_sr) = crate::audio_io::decode_audio_multi(input_audio_path)
            .map_err(|e| format!("decode failed: {e}"))?;
        if channels.is_empty() {
            return Err("decode returned empty channels".to_string());
        }
        if !enabled || bypass {
            if let Some(parent) = output_audio_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            crate::wave::export_channels_audio(&channels, input_sr.max(1), output_audio_path)
                .map_err(|e| format!("encode failed: {e}"))?;
            return Ok(state_blob_b64.map(|s| s.to_string()));
        }

        let bundle = unsafe { PluginBundle::load(plugin_path) }
            .map_err(|e| format!("clap load failed: {e}"))?;
        let factory = bundle
            .get_plugin_factory()
            .ok_or_else(|| "clap missing plugin factory".to_string())?;
        let descriptor = factory
            .plugin_descriptors()
            .next()
            .ok_or_else(|| "clap has no plugin descriptor".to_string())?;
        let plugin_id = descriptor
            .id()
            .ok_or_else(|| "clap descriptor id missing".to_string())?;

        let host_info = HostInfo::new("NeoWaves", "NeoWaves", "https://example.invalid", "0.1.0")
            .map_err(|e| format!("host info failed: {e}"))?;
        let mut instance = PluginInstance::<()>::new(
            |_| (),
            |_| (),
            &bundle,
            plugin_id,
            &host_info,
        )
        .map_err(|e| format!("clap instantiate failed: {e}"))?;

        let block_size = max_block_size.clamp(1, 4096);
        let processor = instance
            .activate(
                |_, _| (),
                PluginAudioConfiguration {
                    sample_rate: sample_rate.max(1) as f64,
                    min_frames_count: 1,
                    max_frames_count: block_size as u32,
                },
            )
            .map_err(|e| format!("clap activate failed: {e}"))?;
        let mut processor = processor
            .start_processing()
            .map_err(|e| format!("clap start_processing failed: {e}"))?;

        let channels_len = channels.len();
        let frames_total = channels[0].len();
        let mut out_channels = vec![vec![0.0f32; frames_total]; channels_len];
        let mut input_ports = AudioPorts::with_capacity(channels_len, 1);
        let mut output_ports = AudioPorts::with_capacity(channels_len, 1);
        let mut cursor = 0usize;
        while cursor < frames_total {
            let frames_now = (frames_total - cursor).min(block_size);
            let mut in_block: Vec<Vec<f32>> = channels
                .iter()
                .map(|ch| ch[cursor..cursor + frames_now].to_vec())
                .collect();
            let mut out_block: Vec<Vec<f32>> = vec![vec![0.0; frames_now]; channels_len];

            let input_events = InputEvents::empty();
            let mut output_events_buf = EventBuffer::new();
            let mut output_events = OutputEvents::from_buffer(&mut output_events_buf);

            let input_audio = input_ports.with_input_buffers([AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_input_only(
                    in_block.iter_mut().map(|buf| InputChannel {
                        buffer: buf.as_mut_slice(),
                        is_constant: false,
                    }),
                ),
            }]);
            let mut output_audio = output_ports.with_output_buffers([AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_output_only(
                    out_block.iter_mut().map(|buf| buf.as_mut_slice()),
                ),
            }]);

            processor
                .process(
                    &input_audio,
                    &mut output_audio,
                    &input_events,
                    &mut output_events,
                    None,
                    None,
                )
                .map_err(|e| format!("clap process failed: {e}"))?;

            for (ci, out) in out_channels.iter_mut().enumerate() {
                out[cursor..cursor + frames_now].copy_from_slice(&out_block[ci]);
            }
            cursor += frames_now;
        }
        let stopped = processor.stop_processing();
        instance.deactivate(stopped);

        if let Some(parent) = output_audio_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        crate::wave::export_channels_audio(&out_channels, sample_rate.max(1), output_audio_path)
            .map_err(|e| format!("encode failed: {e}"))?;

        Ok(state_blob_b64.map(|s| s.to_string()))
    }
}

#[cfg(not(feature = "plugin_native_clap"))]
mod native {
    use super::*;

    pub(crate) fn scan_paths(_search_paths: &[String]) -> Result<Vec<PluginDescriptorInfo>, String> {
        Err("native clap backend unavailable (build without plugin_native_clap)".to_string())
    }

    pub(crate) fn probe(
        _plugin_path: &Path,
    ) -> Result<(PluginDescriptorInfo, Vec<PluginParamInfo>, Option<String>), String> {
        Err("native clap backend unavailable".to_string())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process(
        _plugin_path: &Path,
        _input_audio_path: &Path,
        _output_audio_path: &Path,
        _sample_rate: u32,
        _max_block_size: usize,
        _enabled: bool,
        _bypass: bool,
        _state_blob_b64: Option<&str>,
        _params: &[PluginParamValue],
    ) -> Result<Option<String>, String> {
        Err("native clap backend unavailable".to_string())
    }

    pub(crate) fn is_audio_effect_plugin(_plugin_path: &Path) -> Result<bool, String> {
        Err("native clap backend unavailable".to_string())
    }
}

pub(crate) use native::{is_audio_effect_plugin, probe, process, scan_paths};

#[cfg(test)]
mod tests {
    #[cfg(feature = "plugin_native_clap")]
    #[test]
    fn synth_feature_detection() {
        assert!(super::native::has_synth_like_feature(&[
            "instrument".to_string()
        ]));
        assert!(super::native::has_synth_like_feature(&[
            "audio-effect".to_string(),
            "synthesizer".to_string()
        ]));
        assert!(!super::native::has_synth_like_feature(&[
            "audio-effect".to_string()
        ]));
    }
}
