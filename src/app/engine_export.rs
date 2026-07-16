//! Game-engine metadata export profiles (Wwise / FMOD / Unity).
//!
//! Exports a metadata table only — no audio conversion. Unity and FMOD
//! get JSON an importer script can consume directly; Wwise gets a
//! tab-separated table that pastes into import definitions/spreadsheets.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineProfile {
    Unity,
    Wwise,
    Fmod,
}

impl EngineProfile {
    pub fn label(self) -> &'static str {
        match self {
            EngineProfile::Unity => "Unity (JSON)",
            EngineProfile::Wwise => "Wwise (TSV)",
            EngineProfile::Fmod => "FMOD (JSON)",
        }
    }

    pub fn default_extension(self) -> &'static str {
        match self {
            EngineProfile::Unity | EngineProfile::Fmod => "json",
            EngineProfile::Wwise => "tsv",
        }
    }

    pub fn from_cli_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "unity" => Some(EngineProfile::Unity),
            "wwise" => Some(EngineProfile::Wwise),
            "fmod" => Some(EngineProfile::Fmod),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct EngineExportEntry {
    pub path: PathBuf,
    pub stem: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub total_frames: u64,
    pub length_sec: f64,
    pub loop_points: Option<(u64, u64)>,
    pub lufs: Option<f32>,
}

/// Gather one file's engine metadata from its header + loop markers.
/// `lufs` is passed in (the list's cached measurement, or `None`).
pub fn collect_entry(path: &Path, lufs: Option<f32>) -> anyhow::Result<EngineExportEntry> {
    let info = crate::audio_io::read_audio_info(path)?;
    let total_frames = info.total_frames.unwrap_or(0);
    let sr = info.sample_rate.max(1);
    let length_sec = info
        .duration_secs
        .map(|s| s as f64)
        .unwrap_or(total_frames as f64 / sr as f64);
    Ok(EngineExportEntry {
        path: path.to_path_buf(),
        stem: path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string(),
        sample_rate: sr,
        channels: info.channels as u32,
        total_frames,
        length_sec,
        loop_points: crate::loop_markers::read_loop_markers(path),
        lufs,
    })
}

/// Render `entries` in the given profile's text format.
pub fn render_engine_export(profile: EngineProfile, entries: &[EngineExportEntry]) -> String {
    match profile {
        EngineProfile::Unity => {
            let rows: Vec<serde_json::Value> = entries
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "file": e.path.display().to_string(),
                        "name": e.stem,
                        "loop": e.loop_points.is_some(),
                        "loopStart": e.loop_points.map(|(s, _)| s),
                        "loopEnd": e.loop_points.map(|(_, en)| en),
                        "sampleRate": e.sample_rate,
                        "channels": e.channels,
                        "lengthSec": e.length_sec,
                        "lufs": e.lufs,
                    })
                })
                .collect();
            serde_json::to_string_pretty(&serde_json::Value::Array(rows)).unwrap_or_default()
        }
        EngineProfile::Fmod => {
            let events: Vec<serde_json::Value> = entries
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "event": e.stem,
                        "asset": e.path.display().to_string(),
                        "loopRegion": e.loop_points.map(|(s, en)| serde_json::json!({
                            "startFrames": s,
                            "endFrames": en,
                            "startSec": s as f64 / e.sample_rate.max(1) as f64,
                            "endSec": en as f64 / e.sample_rate.max(1) as f64,
                        })),
                        "sampleRate": e.sample_rate,
                        "channels": e.channels,
                        "lengthSec": e.length_sec,
                        "lufs": e.lufs,
                    })
                })
                .collect();
            serde_json::to_string_pretty(&serde_json::json!({ "events": events }))
                .unwrap_or_default()
        }
        EngineProfile::Wwise => {
            let mut out = String::from(
                "ObjectName\tAudioFile\tLoop\tLoopStart\tLoopEnd\tSampleRate\tChannels\tLengthSec\tLUFS\n",
            );
            for e in entries {
                out.push_str(&format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.3}\t{}\n",
                    e.stem,
                    e.path.display(),
                    if e.loop_points.is_some() { "true" } else { "false" },
                    e.loop_points.map(|(s, _)| s.to_string()).unwrap_or_default(),
                    e.loop_points.map(|(_, en)| en.to_string()).unwrap_or_default(),
                    e.sample_rate,
                    e.channels,
                    e.length_sec,
                    e.lufs.map(|v| format!("{v:.2}")).unwrap_or_default(),
                ));
            }
            out
        }
    }
}

impl crate::app::WavesPreviewer {
    /// Small modal: pick an engine profile and export the metadata table
    /// for the selection (or the whole list).
    pub(crate) fn ui_engine_export_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_engine_export_dialog {
            return;
        }
        let mut open = true;
        let mut do_export = false;
        let target_count = self.inspection_target_paths().len();
        egui::Window::new("Export Engine Metadata")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!(
                    "Exports metadata for {target_count} file(s) (selection, or the whole list when nothing is selected). Audio files are not converted."
                ));
                ui.separator();
                for profile in [EngineProfile::Unity, EngineProfile::Wwise, EngineProfile::Fmod] {
                    ui.radio_value(&mut self.engine_export_profile, profile, profile.label());
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(target_count > 0, egui::Button::new("Export..."))
                        .clicked()
                    {
                        do_export = true;
                    }
                    if ui.button("Cancel").clicked() {
                        self.show_engine_export_dialog = false;
                    }
                });
            });
        if do_export {
            let profile = self.engine_export_profile;
            let Some(out_path) = rfd::FileDialog::new()
                .set_file_name(format!(
                    "engine_metadata.{}",
                    profile.default_extension()
                ))
                .add_filter(profile.default_extension(), &[profile.default_extension()])
                .save_file()
            else {
                return;
            };
            self.show_engine_export_dialog = false;
            match self.write_engine_export(profile, &out_path) {
                Ok(count) => self.push_toast(
                    crate::app::types::ToastSeverity::Info,
                    format!("Exported {count} entries to {}", out_path.display()),
                ),
                Err(err) => self.push_toast(
                    crate::app::types::ToastSeverity::Error,
                    format!("Engine export failed: {err}"),
                ),
            }
        } else if !open {
            self.show_engine_export_dialog = false;
        }
    }

    /// Collect + render + write the export. Returns the entry count.
    pub(super) fn write_engine_export(
        &self,
        profile: EngineProfile,
        out_path: &Path,
    ) -> anyhow::Result<usize> {
        let paths = self.inspection_target_paths();
        let mut entries = Vec::new();
        let mut skipped = 0usize;
        for path in &paths {
            let lufs = self.meta_for_path(path).and_then(|m| m.lufs_i);
            match collect_entry(path, lufs) {
                Ok(entry) => entries.push(entry),
                Err(_) => skipped += 1,
            }
        }
        if entries.is_empty() {
            anyhow::bail!("no readable audio files in the target set ({skipped} skipped)");
        }
        std::fs::write(out_path, render_engine_export(profile, &entries))?;
        Ok(entries.len())
    }

    #[cfg(feature = "kittest")]
    pub fn test_write_engine_export(&self, engine: &str, out_path: &Path) -> Result<usize, String> {
        let profile = EngineProfile::from_cli_name(engine).ok_or("unknown engine")?;
        self.write_engine_export(profile, out_path)
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(stem: &str, looped: bool) -> EngineExportEntry {
        EngineExportEntry {
            path: PathBuf::from(format!("/audio/{stem}.wav")),
            stem: stem.to_string(),
            sample_rate: 48_000,
            channels: 2,
            total_frames: 96_000,
            length_sec: 2.0,
            loop_points: looped.then_some((1_000, 90_000)),
            lufs: Some(-14.2),
        }
    }

    #[test]
    fn unity_json_round_trips_and_carries_loops() {
        let out = render_engine_export(
            EngineProfile::Unity,
            &[entry("se_hit", true), entry("bgm_town", false)],
        );
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        let rows = v.as_array().expect("array");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["name"], "se_hit");
        assert_eq!(rows[0]["loop"], true);
        assert_eq!(rows[0]["loopStart"], 1_000);
        assert_eq!(rows[0]["loopEnd"], 90_000);
        assert_eq!(rows[1]["loop"], false);
        assert!(rows[1]["loopStart"].is_null());
        assert_eq!(rows[0]["sampleRate"], 48_000);
        assert_eq!(rows[0]["lengthSec"], 2.0);
    }

    #[test]
    fn fmod_json_has_events_with_loop_regions_in_seconds() {
        let out = render_engine_export(EngineProfile::Fmod, &[entry("vo_line", true)]);
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        let ev = &v["events"][0];
        assert_eq!(ev["event"], "vo_line");
        let region = &ev["loopRegion"];
        assert_eq!(region["startFrames"], 1_000);
        let start_sec = region["startSec"].as_f64().unwrap();
        assert!((start_sec - 1_000.0 / 48_000.0).abs() < 1e-9);
    }

    #[test]
    fn wwise_tsv_has_header_and_one_row_per_entry() {
        let out = render_engine_export(
            EngineProfile::Wwise,
            &[entry("se_hit", true), entry("amb_wind", false)],
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("ObjectName\tAudioFile\tLoop"));
        let cols: Vec<&str> = lines[1].split('\t').collect();
        assert_eq!(cols[0], "se_hit");
        assert_eq!(cols[2], "true");
        assert_eq!(cols[3], "1000");
        let cols2: Vec<&str> = lines[2].split('\t').collect();
        assert_eq!(cols2[2], "false");
        assert_eq!(cols2[3], "");
    }

    #[test]
    fn cli_profile_names_resolve() {
        assert_eq!(EngineProfile::from_cli_name("unity"), Some(EngineProfile::Unity));
        assert_eq!(EngineProfile::from_cli_name("Wwise"), Some(EngineProfile::Wwise));
        assert_eq!(EngineProfile::from_cli_name("FMOD"), Some(EngineProfile::Fmod));
        assert_eq!(EngineProfile::from_cli_name("unreal"), None);
    }
}
