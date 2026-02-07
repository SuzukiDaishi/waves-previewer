use std::ffi::OsStr;
use std::path::Path;

use crate::plugin::protocol::{PluginFormat, PluginParamInfo};

pub(crate) mod clap;
pub(crate) mod generic;
pub(crate) mod vst3;

pub(crate) fn default_params() -> Vec<PluginParamInfo> {
    vec![
        PluginParamInfo {
            id: "mix".to_string(),
            name: "Mix".to_string(),
            normalized: 1.0,
            default_normalized: 1.0,
            min: 0.0,
            max: 1.0,
            unit: "".to_string(),
        },
        PluginParamInfo {
            id: "output_gain_db".to_string(),
            name: "Output Gain".to_string(),
            normalized: 0.5,
            default_normalized: 0.5,
            min: -24.0,
            max: 24.0,
            unit: "dB".to_string(),
        },
    ]
}

pub(crate) fn resolve_plugin_format(path: &Path) -> Option<PluginFormat> {
    let ext = path
        .extension()
        .and_then(OsStr::to_str)
        .map(|v| v.to_ascii_lowercase())?;
    match ext.as_str() {
        "vst3" => Some(PluginFormat::Vst3),
        "clap" => Some(PluginFormat::Clap),
        _ => None,
    }
}

pub(crate) fn plugin_display_name(path: &Path) -> String {
    path.file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or("(plugin)")
        .to_string()
}
