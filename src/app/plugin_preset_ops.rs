//! Plugin parameter presets: JSON snapshots of a plugin's parameter values
//! and opaque state blob, stored per plugin under
//! `<config>/NeoWaves/plugin_presets/<sanitized_plugin_key>/<name>.json`.
//! Shared by the effect-graph plugin node and the editor's Plugin FX tool.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::app::types::{EffectGraphPluginParamState, PluginParamUiState};

pub const PLUGIN_PRESET_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct PluginPreset {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub plugin_key: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub created_ms: u64,
    #[serde(default)]
    pub params: Vec<EffectGraphPluginParamState>,
    #[serde(default)]
    pub state_blob_b64: Option<String>,
}

/// Filesystem-safe single path component: ASCII alphanumerics, `-`, `_`;
/// anything else becomes `_`. Never empty, bounded length.
pub fn sanitize_preset_component(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    out.truncate(96);
    if out.trim_matches('_').is_empty() {
        out = format!("p{:08x}", fxhash_str(s));
    }
    out
}

/// Tiny deterministic hash so fully-non-ASCII keys still get distinct dirs.
fn fxhash_str(s: &str) -> u32 {
    let mut h = 0x811c_9dc5u32;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Presets directory for one plugin under `root` (created on demand).
pub fn plugin_presets_dir_in(root: &Path, plugin_key: &str) -> PathBuf {
    let mut dir = root.to_path_buf();
    dir.push(sanitize_preset_component(plugin_key));
    dir
}

/// `(display_name, path)` of every readable preset in `dir`, name-sorted.
pub fn list_plugin_presets_in(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        // Prefer the stored display name; fall back to the file stem.
        let name = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<PluginPreset>(&raw).ok())
            .map(|p| p.name)
            .filter(|n| !n.trim().is_empty())
            .or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();
        if !name.is_empty() {
            out.push((name, path));
        }
    }
    out.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    out
}

/// Write `preset` into `dir` as `<sanitized name>.json` (overwrites).
pub fn save_plugin_preset_in(dir: &Path, preset: &PluginPreset) -> Result<PathBuf, String> {
    if preset.name.trim().is_empty() {
        return Err("preset name is empty".to_string());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("create preset dir: {e}"))?;
    let mut path = dir.to_path_buf();
    path.push(format!("{}.json", sanitize_preset_component(preset.name.trim())));
    let json =
        serde_json::to_string_pretty(preset).map_err(|e| format!("serialize preset: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("write preset: {e}"))?;
    Ok(path)
}

pub fn load_plugin_preset_from(path: &Path) -> Result<PluginPreset, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read preset: {e}"))?;
    serde_json::from_str::<PluginPreset>(&raw).map_err(|e| format!("parse preset: {e}"))
}

pub fn delete_plugin_preset_file(path: &Path) -> Result<(), String> {
    std::fs::remove_file(path).map_err(|e| format!("delete preset: {e}"))
}

/// Editor-draft params (UI state) -> preset params.
pub fn preset_params_from_ui(params: &[PluginParamUiState]) -> Vec<EffectGraphPluginParamState> {
    params.iter().map(EffectGraphPluginParamState::from_ui).collect()
}

/// Preset params -> editor-draft params (UI state).
pub fn preset_params_to_ui(params: &[EffectGraphPluginParamState]) -> Vec<PluginParamUiState> {
    params
        .iter()
        .map(|p| PluginParamUiState {
            id: p.id.clone(),
            name: p.name.clone(),
            normalized: p.normalized.clamp(0.0, 1.0),
            default_normalized: p.default_normalized.clamp(0.0, 1.0),
            min: p.min,
            max: p.max,
            unit: p.unit.clone(),
        })
        .collect()
}

impl crate::app::WavesPreviewer {
    /// Root directory for all plugin presets (same config base as the
    /// effect-graph template library).
    pub(super) fn plugin_presets_root() -> Option<PathBuf> {
        let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("LOCALAPPDATA"))?;
        let mut path = PathBuf::from(base);
        path.push("NeoWaves");
        path.push("plugin_presets");
        let _ = std::fs::create_dir_all(&path);
        Some(path)
    }

    pub(super) fn plugin_presets_dir_for_key(plugin_key: &str) -> Option<PathBuf> {
        Some(plugin_presets_dir_in(&Self::plugin_presets_root()?, plugin_key))
    }

    pub(super) fn list_plugin_presets_for_key(plugin_key: &str) -> Vec<(String, PathBuf)> {
        Self::plugin_presets_dir_for_key(plugin_key)
            .map(|dir| list_plugin_presets_in(&dir))
            .unwrap_or_default()
    }

    pub(super) fn save_plugin_preset_for_key(
        plugin_key: &str,
        name: &str,
        params: Vec<EffectGraphPluginParamState>,
        state_blob_b64: Option<String>,
    ) -> Result<PathBuf, String> {
        let dir = Self::plugin_presets_dir_for_key(plugin_key)
            .ok_or_else(|| "could not resolve preset directory".to_string())?;
        let preset = PluginPreset {
            schema_version: PLUGIN_PRESET_SCHEMA_VERSION,
            plugin_key: plugin_key.to_string(),
            name: name.trim().to_string(),
            created_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            params,
            state_blob_b64,
        };
        save_plugin_preset_in(&dir, &preset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "neowaves_plugin_presets_{tag}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp preset root");
        dir
    }

    fn sample_preset(name: &str) -> PluginPreset {
        PluginPreset {
            schema_version: PLUGIN_PRESET_SCHEMA_VERSION,
            plugin_key: "vst3::/plugins/Comp.vst3".to_string(),
            name: name.to_string(),
            created_ms: 12345,
            params: vec![EffectGraphPluginParamState {
                id: "threshold".to_string(),
                name: "Threshold".to_string(),
                normalized: 0.25,
                default_normalized: 0.5,
                min: -60.0,
                max: 0.0,
                unit: "dB".to_string(),
            }],
            state_blob_b64: Some("QUJD".to_string()),
        }
    }

    #[test]
    fn sanitize_makes_safe_nonempty_components() {
        assert_eq!(sanitize_preset_component("My Preset 1"), "My_Preset_1");
        assert_eq!(
            sanitize_preset_component("vst3::/path/To.vst3"),
            "vst3___path_To_vst3"
        );
        // Fully non-ASCII input still yields a usable, deterministic name.
        let a = sanitize_preset_component("プリセット");
        let b = sanitize_preset_component("プリセット");
        assert_eq!(a, b);
        assert!(!a.trim_matches('_').is_empty());
        // Different inputs map to different fallback names.
        assert_ne!(a, sanitize_preset_component("別の名前"));
    }

    #[test]
    fn save_load_roundtrip_preserves_everything() {
        let root = temp_root("roundtrip");
        let dir = plugin_presets_dir_in(&root, "vst3::/plugins/Comp.vst3");
        let preset = sample_preset("Punchy");
        let path = save_plugin_preset_in(&dir, &preset).expect("save");
        let loaded = load_plugin_preset_from(&path).expect("load");
        assert_eq!(loaded.plugin_key, preset.plugin_key);
        assert_eq!(loaded.name, "Punchy");
        assert_eq!(loaded.params, preset.params);
        assert_eq!(loaded.state_blob_b64, preset.state_blob_b64);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn list_returns_sorted_and_delete_removes() {
        let root = temp_root("list");
        let dir = plugin_presets_dir_in(&root, "clap::/plugins/EQ.clap");
        save_plugin_preset_in(&dir, &sample_preset("zeta")).expect("save zeta");
        save_plugin_preset_in(&dir, &sample_preset("Alpha")).expect("save alpha");
        let listed = list_plugin_presets_in(&dir);
        assert_eq!(
            listed.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["Alpha", "zeta"]
        );
        delete_plugin_preset_file(&listed[0].1).expect("delete");
        let listed = list_plugin_presets_in(&dir);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0, "zeta");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_name_is_rejected_and_overwrite_wins() {
        let root = temp_root("names");
        let dir = plugin_presets_dir_in(&root, "k");
        let mut preset = sample_preset("  ");
        assert!(save_plugin_preset_in(&dir, &preset).is_err());
        preset.name = "Same".to_string();
        save_plugin_preset_in(&dir, &preset).expect("first save");
        preset.params[0].normalized = 0.9;
        save_plugin_preset_in(&dir, &preset).expect("overwrite");
        let listed = list_plugin_presets_in(&dir);
        assert_eq!(listed.len(), 1);
        let loaded = load_plugin_preset_from(&listed[0].1).expect("load");
        assert_eq!(loaded.params[0].normalized, 0.9);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ui_param_conversion_roundtrips() {
        let ui = vec![PluginParamUiState {
            id: "gain".to_string(),
            name: "Gain".to_string(),
            normalized: 0.75,
            default_normalized: 0.5,
            min: -24.0,
            max: 24.0,
            unit: "dB".to_string(),
        }];
        let graph = preset_params_from_ui(&ui);
        let back = preset_params_to_ui(&graph);
        assert_eq!(ui, back);
    }
}
