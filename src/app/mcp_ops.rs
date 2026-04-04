use crate::mcp;
use std::path::PathBuf;

use super::types::{ConflictPolicy, MediaId, MediaSource, RateMode, SaveMode};
use super::WavesPreviewer;

impl WavesPreviewer {
    pub(super) fn process_mcp_commands(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.mcp_cmd_rx else {
            return;
        };
        let Some(tx) = self.mcp_resp_tx.clone() else {
            return;
        };
        let mut cmds = Vec::new();
        while let Ok(cmd) = rx.try_recv() {
            cmds.push(cmd);
        }
        for cmd in cmds {
            let res = self.handle_mcp_command(cmd, ctx);
            let _ = tx.send(res);
        }
    }

    fn handle_mcp_command(
        &mut self,
        cmd: mcp::UiCommand,
        ctx: &egui::Context,
    ) -> mcp::UiCommandResult {
        use serde_json::{json, to_value, Value};
        let ok = |payload: Value| mcp::UiCommandResult {
            ok: true,
            payload,
            error: None,
        };
        let err = |msg: String| mcp::UiCommandResult {
            ok: false,
            payload: Value::Null,
            error: Some(msg),
        };
        match cmd {
            mcp::UiCommand::ListFiles(args) => match self.mcp_list_files(args) {
                Ok(res) => ok(to_value(res).unwrap_or(Value::Null)),
                Err(e) => err(e),
            },
            mcp::UiCommand::GetSelection => {
                let selected_paths: Vec<String> = self
                    .selected_paths()
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                let active_tab_path = self
                    .active_tab
                    .and_then(|i| self.tabs.get(i))
                    .map(|t| t.path.display().to_string());
                ok(to_value(mcp::types::SelectionResult {
                    selected_paths,
                    active_tab_path,
                })
                .unwrap_or(Value::Null))
            }
            mcp::UiCommand::SetSelection(args) => {
                let mut found_rows: Vec<usize> = Vec::new();
                for p in &args.paths {
                    let path = PathBuf::from(p);
                    if let Some(row) = self.row_for_path(&path) {
                        found_rows.push(row);
                    }
                }
                if found_rows.is_empty() {
                    return err("NOT_FOUND: no matching paths in list".to_string());
                }
                found_rows.sort_unstable();
                self.selected_multi.clear();
                for row in &found_rows {
                    self.selected_multi.insert(*row);
                }
                self.selected = Some(found_rows[0]);
                self.select_anchor = Some(found_rows[0]);
                self.select_and_load(found_rows[0], true);
                if args.open_tab.unwrap_or(false) {
                    if let Some(path) = self.path_for_row(found_rows[0]).cloned() {
                        self.open_or_activate_tab(&path);
                    }
                }
                let selected_paths: Vec<String> = found_rows
                    .iter()
                    .filter_map(|row| self.path_for_row(*row))
                    .map(|p| p.display().to_string())
                    .collect();
                let active_tab_path = self
                    .active_tab
                    .and_then(|i| self.tabs.get(i))
                    .map(|t| t.path.display().to_string());
                ok(to_value(mcp::types::SelectionResult {
                    selected_paths,
                    active_tab_path,
                })
                .unwrap_or(Value::Null))
            }
            mcp::UiCommand::Play => {
                let playing = self
                    .audio
                    .shared
                    .playing
                    .load(std::sync::atomic::Ordering::Relaxed);
                if !playing {
                    self.request_workspace_play_toggle();
                }
                ok(json!({"ok": true}))
            }
            mcp::UiCommand::Stop => {
                let playing = self
                    .audio
                    .shared
                    .playing
                    .load(std::sync::atomic::Ordering::Relaxed);
                if playing {
                    self.audio.stop();
                    self.list_play_pending = false;
                }
                ok(json!({"ok": true}))
            }
            mcp::UiCommand::SetVolume(args) => {
                self.volume_db = args.db;
                self.apply_effective_volume();
                ok(json!({"ok": true}))
            }
            mcp::UiCommand::SetMode(args) => {
                let prev = self.mode;
                let prev_rate = self.playback_rate;
                self.mode = match args.mode.as_str() {
                    "Speed" => RateMode::Speed,
                    "PitchShift" => RateMode::PitchShift,
                    "TimeStretch" => RateMode::TimeStretch,
                    _ => prev,
                };
                if self.mode != prev {
                    self.refresh_playback_mode_for_current_source(prev, prev_rate);
                }
                ok(json!({"ok": true}))
            }
            mcp::UiCommand::SetSpeed(args) => {
                let prev_rate = self.playback_rate;
                self.playback_rate = args.rate;
                match self.mode {
                    RateMode::Speed | RateMode::TimeStretch => {
                        self.refresh_playback_mode_for_current_source(self.mode, prev_rate);
                    }
                    _ => {}
                }
                ok(json!({"ok": true}))
            }
            mcp::UiCommand::SetPitch(args) => {
                let prev_rate = self.playback_rate;
                self.pitch_semitones = args.semitones;
                if self.mode == RateMode::PitchShift {
                    self.refresh_playback_mode_for_current_source(self.mode, prev_rate);
                }
                ok(json!({"ok": true}))
            }
            mcp::UiCommand::SetStretch(args) => {
                let prev_rate = self.playback_rate;
                self.playback_rate = args.rate;
                if self.mode == RateMode::TimeStretch {
                    self.refresh_playback_mode_for_current_source(self.mode, prev_rate);
                }
                ok(json!({"ok": true}))
            }
            mcp::UiCommand::ApplyGain(args) => {
                let path = PathBuf::from(args.path);
                if self.path_index.contains_key(&path) {
                    let new = Self::clamp_gain_db(args.db);
                    self.set_pending_gain_db_for_path(&path, new);
                    if self.playing_path.as_ref() == Some(&path) {
                        self.apply_effective_volume();
                    }
                    self.schedule_lufs_for_path(path);
                    ok(json!({"ok": true}))
                } else {
                    err("NOT_FOUND: file not in list".to_string())
                }
            }
            mcp::UiCommand::ClearGain(args) => {
                let path = PathBuf::from(args.path);
                if self.path_index.contains_key(&path) {
                    self.set_pending_gain_db_for_path(&path, 0.0);
                    self.lufs_override.remove(&path);
                    self.lufs_recalc_deadline.remove(&path);
                    if self.playing_path.as_ref() == Some(&path) {
                        self.apply_effective_volume();
                    }
                    ok(json!({"ok": true}))
                } else {
                    err("NOT_FOUND: file not in list".to_string())
                }
            }
            mcp::UiCommand::SetLoopMarkers(args) => {
                let path = PathBuf::from(args.path);
                if let Some(idx) = self.tabs.iter().position(|t| t.path == path) {
                    if let Some(tab) = self.tabs.get_mut(idx) {
                        let s = args.start_samples as usize;
                        let e = args.end_samples as usize;
                        if s < e && e <= tab.samples_len {
                            tab.loop_region = Some((s, e));
                            Self::update_loop_markers_dirty(tab);
                        }
                    }
                    ok(json!({"ok": true}))
                } else {
                    err("NOT_FOUND: tab not open".to_string())
                }
            }
            mcp::UiCommand::WriteLoopMarkers(args) => {
                let path = PathBuf::from(args.path);
                if let Some(idx) = self.tabs.iter().position(|t| t.path == path) {
                    if self.write_loop_markers_for_tab(idx) {
                        ok(json!({"ok": true}))
                    } else {
                        err("FAILED: write loop markers".to_string())
                    }
                } else {
                    err("NOT_FOUND: tab not open".to_string())
                }
            }
            mcp::UiCommand::Export(args) => {
                match args.mode.as_str() {
                    "Overwrite" => self.export_cfg.save_mode = SaveMode::Overwrite,
                    "NewFile" => self.export_cfg.save_mode = SaveMode::NewFile,
                    _ => {}
                }
                if let Some(dest) = args.dest_folder {
                    self.export_cfg.dest_folder = Some(PathBuf::from(dest));
                }
                if let Some(template) = args.name_template {
                    self.export_cfg.name_template = template;
                }
                if let Some(conflict) = args.conflict {
                    self.export_cfg.conflict = match conflict.as_str() {
                        "Overwrite" => ConflictPolicy::Overwrite,
                        "Skip" => ConflictPolicy::Skip,
                        _ => ConflictPolicy::Rename,
                    };
                }
                self.export_cfg.first_prompt = false;
                self.trigger_save_selected();
                ok(json!({"queued": true}))
            }
            mcp::UiCommand::OpenFolder(args) => {
                let path = PathBuf::from(args.path);
                if path.is_dir() {
                    self.root = Some(path);
                    self.rescan();
                    ok(json!({"ok": true}))
                } else {
                    err("NOT_FOUND: folder not found".to_string())
                }
            }
            mcp::UiCommand::OpenFiles(args) => {
                let paths: Vec<PathBuf> = args.paths.into_iter().map(PathBuf::from).collect();
                self.replace_with_files(&paths);
                self.after_add_refresh();
                ok(json!({"ok": true}))
            }
            mcp::UiCommand::Screenshot(args) => {
                let path = args
                    .path
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.default_screenshot_path());
                self.request_screenshot(ctx, path.clone(), false);
                ok(json!({"path": path.display().to_string()}))
            }
            mcp::UiCommand::DebugSummary => {
                let selected_paths: Vec<String> = self
                    .selected_paths()
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                let active_tab_path = self
                    .active_tab
                    .and_then(|i| self.tabs.get(i))
                    .map(|t| t.path.display().to_string());
                let mode = Some(format!("{:?}", self.mode));
                let playing = self
                    .audio
                    .shared
                    .playing
                    .load(std::sync::atomic::Ordering::Relaxed);
                ok(to_value(mcp::types::DebugSummary {
                    selected_paths,
                    active_tab_path,
                    mode,
                    playing,
                })
                .unwrap_or(Value::Null))
            }
        }
    }

    pub(super) fn mcp_list_files(
        &self,
        args: mcp::types::ListFilesArgs,
    ) -> std::result::Result<mcp::types::ListFilesResult, String> {
        use regex::RegexBuilder;
        let query = args.query.unwrap_or_default();
        let query = query.trim().to_string();
        let use_regex = args.regex.unwrap_or(false);
        let mut ids: Vec<MediaId> = self.files.clone();
        ids.retain(|id| {
            self.item_for_id(*id)
                .map(|item| item.source == MediaSource::File)
                .unwrap_or(false)
        });
        if !query.is_empty() {
            let re = if use_regex {
                RegexBuilder::new(&query)
                    .case_insensitive(true)
                    .build()
                    .ok()
            } else {
                RegexBuilder::new(&regex::escape(&query))
                    .case_insensitive(true)
                    .build()
                    .ok()
            };
            ids.retain(|id| {
                let Some(item) = self.item_for_id(*id) else {
                    return false;
                };
                let name = item.display_name.as_str();
                let parent = item.display_folder.as_str();
                let transcript = item
                    .transcript
                    .as_ref()
                    .map(|t| t.full_text.as_str())
                    .unwrap_or("");
                let external_hit = item.external.values().any(|v| {
                    if let Some(re) = re.as_ref() {
                        re.is_match(v)
                    } else {
                        false
                    }
                });
                if let Some(re) = re.as_ref() {
                    re.is_match(name)
                        || re.is_match(parent)
                        || re.is_match(transcript)
                        || external_hit
                } else {
                    false
                }
            });
        }
        let total = ids.len() as u32;
        let offset = args.offset.unwrap_or(0) as usize;
        let limit = args.limit.unwrap_or(u32::MAX) as usize;
        let include_meta = args.include_meta.unwrap_or(true);
        let mut items = Vec::new();
        for id in ids.into_iter().skip(offset).take(limit) {
            let Some(item) = self.item_for_id(id) else {
                continue;
            };
            let path = item.path.display().to_string();
            let name = item.display_name.clone();
            let folder = item.display_folder.clone();
            let meta = if include_meta {
                item.meta.as_ref()
            } else {
                None
            };
            let status = if !item.path.exists() {
                Some("missing".to_string())
            } else if let Some(m) = item.meta.as_ref() {
                if m.decode_error.is_some() {
                    Some("decode_failed".to_string())
                } else {
                    Some("ok".to_string())
                }
            } else {
                None
            };
            items.push(mcp::types::FileItem {
                path,
                name,
                folder,
                length_secs: meta.and_then(|m| m.duration_secs),
                sample_rate: meta.map(|m| m.sample_rate),
                channels: meta.map(|m| m.channels),
                bits: meta.map(|m| m.bits_per_sample),
                peak_db: meta.and_then(|m| m.peak_db),
                lufs_i: meta.and_then(|m| m.lufs_i),
                gain_db: Some(item.pending_gain_db),
                status,
            });
        }
        Ok(mcp::types::ListFilesResult { total, items })
    }
}
