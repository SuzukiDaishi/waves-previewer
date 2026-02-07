use std::fs;
use std::path::Path;

use base64::Engine;

use crate::plugin::backends::{default_params, plugin_display_name, resolve_plugin_format};
use crate::plugin::protocol::{
    PluginDescriptorInfo, PluginFormat, PluginParamInfo, PluginParamValue,
};

fn scan_path_by_extension(path: &Path, out: &mut Vec<PluginDescriptorInfo>) {
    if !path.exists() {
        return;
    }
    if let Some(format) = resolve_plugin_format(path) {
        let path_str = path.to_string_lossy().to_string();
        out.push(PluginDescriptorInfo {
            key: path_str.clone(),
            name: plugin_display_name(path),
            path: path_str,
            format,
        });
        return;
    }
    let walker = walkdir::WalkDir::new(path)
        .follow_links(false)
        .max_depth(8)
        .into_iter();
    for entry in walker.filter_map(Result::ok) {
        let p = entry.path();
        if let Some(format) = resolve_plugin_format(p) {
            let path_str = p.to_string_lossy().to_string();
            out.push(PluginDescriptorInfo {
                key: path_str.clone(),
                name: plugin_display_name(p),
                path: path_str,
                format,
            });
        }
    }
}

fn parse_param(params: &[PluginParamValue], id: &str, default: f32) -> f32 {
    params
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.normalized.clamp(0.0, 1.0))
        .unwrap_or(default)
}

fn find_param(params: &[PluginParamValue], id: &str) -> Option<f32> {
    params
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.normalized.clamp(0.0, 1.0))
}

fn param_to_gain_db(norm: f32) -> f32 {
    -24.0 + (norm.clamp(0.0, 1.0) * 48.0)
}

fn has_ott_signature(params: &[PluginParamValue]) -> bool {
    find_param(params, "vst3:00000000").is_some()
        && find_param(params, "vst3:00000013").is_some()
}

fn norm_to_db(norm: f32, min_db: f32, max_db: f32) -> f32 {
    min_db + (max_db - min_db) * norm.clamp(0.0, 1.0)
}

fn apply_peak_limit(channels: &mut [Vec<f32>], ceiling: f32) {
    let mut peak = 0.0f32;
    for ch in channels.iter() {
        for &v in ch {
            peak = peak.max(v.abs());
        }
    }
    if peak <= ceiling || peak <= 1e-9 {
        return;
    }
    let scale = ceiling / peak;
    for ch in channels.iter_mut() {
        for v in ch.iter_mut() {
            *v *= scale;
        }
    }
}

fn apply_ott_fallback(channels: &mut [Vec<f32>], params: &[PluginParamValue]) {
    let depth = find_param(params, "vst3:00000000").unwrap_or(1.0);
    let bypass = find_param(params, "vst3:00000013").unwrap_or(0.0) >= 0.5;
    if bypass {
        return;
    }

    // Conservative mapping to avoid the fallback saturating into a near-square waveform.
    let in_gain_db = norm_to_db(find_param(params, "vst3:00000002").unwrap_or(0.5), -12.0, 12.0);
    let out_gain_db = norm_to_db(find_param(params, "vst3:00000003").unwrap_or(0.5), -12.0, 12.0);
    let in_gain = 10.0f32.powf(in_gain_db / 20.0);
    let out_gain = 10.0f32.powf(out_gain_db / 20.0);
    let shape = 1.0 + depth * 1.5; // 1.0..2.5
    let wet_mix = (0.15 + depth * 0.5).clamp(0.0, 0.65);

    for ch in channels.iter_mut() {
        for s in ch.iter_mut() {
            let dry = *s;
            let x = (dry * in_gain).clamp(-2.0, 2.0);
            let wet = ((x * shape).tanh() / shape).clamp(-1.0, 1.0) * out_gain;
            *s = (dry * (1.0 - wet_mix) + wet * wet_mix).clamp(-1.0, 1.0);
        }
    }

    // Keep fallback output headroom predictable for UI preview and apply.
    apply_peak_limit(channels, 0.98);
}

pub(crate) fn state_blob_b64(params: &[PluginParamValue]) -> Option<String> {
    serde_json::to_vec(params)
        .ok()
        .map(|bytes| base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes))
}

pub(crate) fn default_probe_result(
    plugin_path: &Path,
    format: PluginFormat,
) -> (PluginDescriptorInfo, Vec<PluginParamInfo>, Option<String>) {
    let path_str = plugin_path.to_string_lossy().to_string();
    (
        PluginDescriptorInfo {
            key: path_str.clone(),
            name: plugin_display_name(plugin_path),
            path: path_str,
            format,
        },
        default_params(),
        None,
    )
}

pub(crate) fn scan_paths(search_paths: &[String]) -> Vec<PluginDescriptorInfo> {
    let mut out = Vec::new();
    for raw in search_paths {
        scan_path_by_extension(Path::new(raw), &mut out);
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

pub(crate) fn process(
    input_path: &Path,
    output_path: &Path,
    enabled: bool,
    bypass: bool,
    params: &[PluginParamValue],
) -> Result<(), String> {
    let (mut channels, sample_rate) = crate::audio_io::decode_audio_multi(input_path)
        .map_err(|e| format!("decode failed: {e}"))?;
    if enabled && !bypass {
        if has_ott_signature(params) {
            apply_ott_fallback(&mut channels, params);
        } else {
            let mix = parse_param(params, "mix", 1.0);
            let gain_db = param_to_gain_db(parse_param(params, "output_gain_db", 0.5));
            let gain = 10.0f32.powf(gain_db / 20.0);
            for ch in &mut channels {
                for v in ch.iter_mut() {
                    let dry = *v;
                    let wet = (dry * gain).clamp(-1.0, 1.0);
                    *v = (dry * (1.0 - mix) + wet * mix).clamp(-1.0, 1.0);
                }
            }
        }
    }
    if let Some(parent) = output_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    crate::wave::export_channels_audio(&channels, sample_rate.max(1), output_path)
        .map_err(|e| format!("encode failed: {e}"))?;
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn normalized_param_summary(params: &[PluginParamInfo]) -> Vec<(String, f32)> {
    params
        .iter()
        .map(|p| {
            let n = p.normalized.clamp(0.0, 1.0);
            let actual = p.min + (p.max - p.min) * n;
            (p.id.clone(), actual)
        })
        .collect()
}

pub(crate) fn format_from_path(plugin_path: &Path) -> Option<PluginFormat> {
    resolve_plugin_format(plugin_path)
}
