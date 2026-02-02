use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::audio::AudioBuffer;
use crate::ipc;

use super::external_ops;
use super::project::{
    deserialize_project, describe_missing, fade_shape_from_str, load_sidecar_audio,
    loop_mode_from_str, loop_shape_from_str, marker_entry_to_project, missing_file_meta,
    project_channel_view_to_channel_view, project_marker_to_entry, project_spectrogram_from_cfg,
    project_tab_from_tab, project_tool_state_to_tool_state, rel_path, resolve_path,
    save_sidecar_audio, save_sidecar_cached_audio, save_sidecar_preview_audio, serialize_project,
    spectro_config_from_project, tool_kind_from_str, view_mode_from_str, ProjectApp, ProjectEdit,
    ProjectFile, ProjectList, ProjectListColumns, ProjectListItem, ProjectToolState,
};
use super::types::{LoopXfadeShape, MediaSource};

pub(super) struct ProjectOpenState {
    pub started_at: Instant,
    pub shown: bool,
}

impl super::WavesPreviewer {
    pub(super) fn queue_project_open(&mut self, path: PathBuf) {
        self.project_open_pending = Some(path);
        self.project_open_state = Some(ProjectOpenState {
            started_at: Instant::now(),
            shown: false,
        });
    }

    pub(super) fn tick_project_open(&mut self) {
        let Some(state) = self.project_open_state.as_mut() else {
            return;
        };
        if !state.shown {
            state.shown = true;
            return;
        }
        let Some(path) = self.project_open_pending.take() else {
            self.project_open_state = None;
            return;
        };
        if let Err(err) = self.open_project_file(path) {
            self.debug_log(format!("session open error: {err}"));
        }
        self.project_open_state = None;
    }

    pub(super) fn is_session_path(path: &Path) -> bool {
        path.extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("nwsess") || s.eq_ignore_ascii_case("nwproj"))
            .unwrap_or(false)
    }

    pub(super) fn save_project(&mut self) -> Result<(), String> {
        let path = match self.project_path.clone() {
            Some(p) => p,
            None => {
                let Some(mut picked) = self.pick_project_save_dialog() else {
                    return Ok(());
                };
                let needs_ext = picked
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| !s.eq_ignore_ascii_case("nwsess"))
                    .unwrap_or(true);
                if needs_ext {
                    picked.set_extension("nwsess");
                }
                picked
            }
        };
        let path = if path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("nwproj"))
            .unwrap_or(false)
        {
            path.with_extension("nwsess")
        } else {
            path
        };
        self.save_project_as(path)
    }

    pub(super) fn save_project_as(&mut self, path: PathBuf) -> Result<(), String> {
        let path = if path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("nwproj"))
            .unwrap_or(false)
        {
            path.with_extension("nwsess")
        } else {
            path
        };
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let list_files: Vec<PathBuf> = self.items.iter().map(|i| i.path.clone()).collect();
        let mut list_items = Vec::new();
        for item in &self.items {
            if item.pending_gain_db.abs() > 0.0001 {
                list_items.push(ProjectListItem {
                    path: rel_path(&item.path, base_dir),
                    pending_gain_db: item.pending_gain_db,
                });
            }
        }
        let list = ProjectList {
            root: self.root.as_ref().map(|p| rel_path(p, base_dir)),
            files: list_files.iter().map(|p| rel_path(p, base_dir)).collect(),
            items: list_items,
        };
        let app = ProjectApp {
            theme: match self.theme_mode {
                super::types::ThemeMode::Light => "light".to_string(),
                _ => "dark".to_string(),
            },
            sort_key: match self.sort_key {
                super::types::SortKey::File => "File",
                super::types::SortKey::Folder => "Folder",
                super::types::SortKey::Transcript => "Transcript",
                super::types::SortKey::Length => "Length",
                super::types::SortKey::Channels => "Channels",
                super::types::SortKey::SampleRate => "SampleRate",
                super::types::SortKey::Bits => "Bits",
                super::types::SortKey::BitRate => "BitRate",
                super::types::SortKey::Level => "Level",
                super::types::SortKey::Lufs => "Lufs",
                super::types::SortKey::Bpm => "Bpm",
                super::types::SortKey::CreatedAt => "CreatedAt",
                super::types::SortKey::ModifiedAt => "ModifiedAt",
                super::types::SortKey::External(_) => "External",
            }
            .to_string(),
            sort_dir: match self.sort_dir {
                super::types::SortDir::Asc => "Asc",
                super::types::SortDir::Desc => "Desc",
                super::types::SortDir::None => "None",
            }
            .to_string(),
            search_query: self.search_query.clone(),
            search_regex: self.search_use_regex,
            list_columns: ProjectListColumns {
                edited: self.list_columns.edited,
                file: self.list_columns.file,
                folder: self.list_columns.folder,
                transcript: self.list_columns.transcript,
                external: self.list_columns.external,
                length: self.list_columns.length,
                ch: self.list_columns.channels,
                sr: self.list_columns.sample_rate,
                bits: self.list_columns.bits,
                bit_rate: self.list_columns.bit_rate,
                peak: self.list_columns.peak,
                lufs: self.list_columns.lufs,
                bpm: self.list_columns.bpm,
                created_at: self.list_columns.created_at,
                modified_at: self.list_columns.modified_at,
                gain: self.list_columns.gain,
                wave: self.list_columns.wave,
            },
        };
        let spectrogram = project_spectrogram_from_cfg(&self.spectro_cfg);

        let mut tabs = Vec::new();
        for (idx, tab) in self.tabs.iter().enumerate() {
            let mut edited_audio = None;
            let mut preview_audio = None;
            let mut preview_tool = None;
            if tab.dirty && !tab.ch_samples.is_empty() {
                match save_sidecar_audio(
                    &path,
                    idx,
                    &tab.ch_samples,
                    self.audio.shared.out_sample_rate,
                ) {
                    Ok(p) => {
                        edited_audio = Some(p);
                    }
                    Err(err) => {
                        return Err(format!("Failed to save edited audio: {err}"));
                    }
                }
            }
            if let Some(overlay) = tab.preview_overlay.as_ref() {
                if !overlay.channels.is_empty() {
                    match save_sidecar_preview_audio(
                        &path,
                        idx,
                        &overlay.channels,
                        self.audio.shared.out_sample_rate,
                    ) {
                        Ok(p) => {
                            preview_audio = Some(p);
                            preview_tool = Some(format!("{:?}", overlay.source_tool));
                        }
                        Err(err) => {
                            return Err(format!("Failed to save preview audio: {err}"));
                        }
                    }
                }
            } else if let Some(tool) = tab.preview_audio_tool {
                preview_tool = Some(format!("{:?}", tool));
            }
            let entry =
                project_tab_from_tab(tab, base_dir, edited_audio, preview_audio, preview_tool);
            tabs.push(entry);
        }

        let mut cached_edits = Vec::new();
        for (idx, (item_path, cached)) in self.edited_cache.iter().enumerate() {
            if cached.ch_samples.is_empty() {
                continue;
            }
            let edited_audio = match save_sidecar_cached_audio(
                &path,
                idx,
                &cached.ch_samples,
                self.audio.shared.out_sample_rate,
            ) {
                Ok(p) => p,
                Err(err) => {
                    return Err(format!("Failed to save cached audio: {err}"));
                }
            };
            cached_edits.push(ProjectEdit {
                path: rel_path(item_path, base_dir),
                edited_audio: rel_path(&edited_audio, base_dir),
                dirty: cached.dirty,
                loop_region: cached.loop_region.map(|v| [v.0, v.1]),
                loop_markers_saved: cached.loop_markers_saved.map(|v| [v.0, v.1]),
                loop_markers_dirty: cached.loop_markers_dirty,
                markers: cached.markers.iter().map(marker_entry_to_project).collect(),
                markers_saved: cached
                    .markers_saved
                    .iter()
                    .map(marker_entry_to_project)
                    .collect(),
                markers_dirty: cached.markers_dirty,
                trim_range: cached.trim_range.map(|v| [v.0, v.1]),
                loop_xfade_samples: cached.loop_xfade_samples,
                loop_xfade_shape: match cached.loop_xfade_shape {
                    LoopXfadeShape::Linear => "linear",
                    LoopXfadeShape::EqualPower => "equal",
                }
                .to_string(),
                fade_in_range: cached.fade_in_range.map(|v| [v.0, v.1]),
                fade_out_range: cached.fade_out_range.map(|v| [v.0, v.1]),
                fade_in_shape: format!("{:?}", cached.fade_in_shape),
                fade_out_shape: format!("{:?}", cached.fade_out_shape),
                loop_mode: format!("{:?}", cached.loop_mode),
                snap_zero_cross: cached.snap_zero_cross,
                tool_state: ProjectToolState {
                    fade_in_ms: cached.tool_state.fade_in_ms,
                    fade_out_ms: cached.tool_state.fade_out_ms,
                    gain_db: cached.tool_state.gain_db,
                    normalize_target_db: cached.tool_state.normalize_target_db,
                    loudness_target_lufs: cached.tool_state.loudness_target_lufs,
                    pitch_semitones: cached.tool_state.pitch_semitones,
                    stretch_rate: cached.tool_state.stretch_rate,
                    loop_repeat: cached.tool_state.loop_repeat,
                },
                active_tool: format!("{:?}", cached.active_tool),
                show_waveform_overlay: cached.show_waveform_overlay,
                bpm_enabled: cached.bpm_enabled,
                bpm_value: cached.bpm_value,
                bpm_user_set: cached.bpm_user_set,
            });
        }

        let project = ProjectFile {
            version: 1,
            name: path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string()),
            base_dir: Some(base_dir.to_string_lossy().to_string()),
            list,
            app,
            spectrogram,
            tabs,
            active_tab: self.active_tab,
            cached_edits,
        };
        let text = serialize_project(&project).map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, text).map_err(|e| e.to_string())?;
        self.project_path = Some(path);
        Ok(())
    }

    pub(super) fn open_project_file(&mut self, path: PathBuf) -> Result<(), String> {
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let project = deserialize_project(&text).map_err(|e| e.to_string())?;
        if project.version != 1 {
            return Err(format!("Unsupported session version: {}", project.version));
        }
        let base_dir = if let Some(base) = project.base_dir.as_ref() {
            let base_path = PathBuf::from(base);
            if base_path.is_absolute() {
                base_path
            } else {
                path.parent().unwrap_or_else(|| Path::new(".")).join(base_path)
            }
        } else {
            path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf()
        };

        let project_path = path.clone();
        self.close_project();
        self.project_path = Some(project_path.clone());

        self.search_query = project.app.search_query.clone();
        self.search_use_regex = project.app.search_regex;
        self.list_columns = super::types::ListColumnConfig {
            edited: project.app.list_columns.edited,
            file: project.app.list_columns.file,
            folder: project.app.list_columns.folder,
            transcript: project.app.list_columns.transcript,
            external: project.app.list_columns.external,
            length: project.app.list_columns.length,
            channels: project.app.list_columns.ch,
            sample_rate: project.app.list_columns.sr,
            bits: project.app.list_columns.bits,
            bit_rate: project.app.list_columns.bit_rate,
            peak: project.app.list_columns.peak,
            lufs: project.app.list_columns.lufs,
            bpm: project.app.list_columns.bpm,
            created_at: project.app.list_columns.created_at,
            modified_at: project.app.list_columns.modified_at,
            gain: project.app.list_columns.gain,
            wave: project.app.list_columns.wave,
        };
        self.sort_key = match project.app.sort_key.as_str() {
            "Folder" => super::types::SortKey::Folder,
            "Transcript" => super::types::SortKey::Transcript,
            "Length" => super::types::SortKey::Length,
            "Channels" => super::types::SortKey::Channels,
            "SampleRate" => super::types::SortKey::SampleRate,
            "Bits" => super::types::SortKey::Bits,
            "BitRate" => super::types::SortKey::BitRate,
            "Level" => super::types::SortKey::Level,
            "Lufs" => super::types::SortKey::Lufs,
            "Bpm" => super::types::SortKey::Bpm,
            "CreatedAt" => super::types::SortKey::CreatedAt,
            "ModifiedAt" => super::types::SortKey::ModifiedAt,
            _ => super::types::SortKey::File,
        };
        self.sort_dir = match project.app.sort_dir.as_str() {
            "Asc" => super::types::SortDir::Asc,
            "Desc" => super::types::SortDir::Desc,
            _ => super::types::SortDir::None,
        };
        match project.app.theme.as_str() {
            "light" => self.theme_mode = super::types::ThemeMode::Light,
            _ => self.theme_mode = super::types::ThemeMode::Dark,
        }
        self.apply_spectro_config(spectro_config_from_project(&project.spectrogram));

        if !project.list.files.is_empty() {
            self.reset_list_from_project(&project.list.files, &base_dir);
            self.after_add_refresh();
        } else if let Some(root) = project.list.root.as_ref() {
            let root_path = resolve_path(root, &base_dir);
            self.root = Some(root_path);
            self.rescan();
        }

        for item in project.list.items.iter() {
            let path = resolve_path(&item.path, &base_dir);
            if let Some(list_item) = self.item_for_path_mut(&path) {
                list_item.pending_gain_db = item.pending_gain_db;
            }
        }

        let out_sr = self.audio.shared.out_sample_rate;
        for edit in project.cached_edits.iter() {
            let path = resolve_path(&edit.path, &base_dir);
            let edited = load_sidecar_audio(&project_path, &edit.edited_audio).ok();
            let Some((mut chans, sr, _)) = edited else {
                continue;
            };
            if sr != out_sr {
                for ch in chans.iter_mut() {
                    *ch = crate::wave::resample_linear(ch, sr, out_sr);
                }
            }
            let samples_len = chans.get(0).map(|c| c.len()).unwrap_or(0);
            let mut waveform = Vec::new();
            let mono = super::WavesPreviewer::mixdown_channels(&chans, samples_len);
            crate::wave::build_minmax(&mut waveform, &mono, 2048);
            self.edited_cache.insert(
                path,
                super::types::CachedEdit {
                    ch_samples: chans,
                    samples_len,
                    waveform_minmax: waveform,
                    dirty: edit.dirty,
                    loop_region: edit.loop_region.map(|v| (v[0], v[1])),
                    loop_region_committed: edit.loop_region.map(|v| (v[0], v[1])),
                    loop_region_applied: edit.loop_region.map(|v| (v[0], v[1])),
                    loop_markers_saved: edit.loop_markers_saved.map(|v| (v[0], v[1])),
                    loop_markers_dirty: edit.loop_markers_dirty,
                    markers: edit.markers.iter().map(project_marker_to_entry).collect(),
                    markers_committed: edit.markers.iter().map(project_marker_to_entry).collect(),
                    markers_applied: edit.markers.iter().map(project_marker_to_entry).collect(),
                    markers_saved: edit
                        .markers_saved
                        .iter()
                        .map(project_marker_to_entry)
                        .collect(),
                    markers_dirty: edit.markers_dirty,
                    trim_range: edit.trim_range.map(|v| (v[0], v[1])),
                    loop_xfade_samples: edit.loop_xfade_samples,
                    loop_xfade_shape: loop_shape_from_str(&edit.loop_xfade_shape),
                    fade_in_range: edit.fade_in_range.map(|v| (v[0], v[1])),
                    fade_out_range: edit.fade_out_range.map(|v| (v[0], v[1])),
                    fade_in_shape: fade_shape_from_str(&edit.fade_in_shape),
                    fade_out_shape: fade_shape_from_str(&edit.fade_out_shape),
                    loop_mode: loop_mode_from_str(&edit.loop_mode),
                    snap_zero_cross: edit.snap_zero_cross,
                    tool_state: project_tool_state_to_tool_state(&edit.tool_state),
                    active_tool: tool_kind_from_str(&edit.active_tool),
                    show_waveform_overlay: edit.show_waveform_overlay,
                    bpm_enabled: edit.bpm_enabled,
                    bpm_value: edit.bpm_value,
                    bpm_user_set: edit.bpm_user_set,
                },
            );
        }

        for tab in project.tabs.iter() {
            let tab_path = resolve_path(&tab.path, &base_dir);
            let edited = if let Some(raw) = tab.edited_audio.as_ref() {
                load_sidecar_audio(&project_path, raw).ok()
            } else {
                None
            };
            if let Some((mut chans, sr, _)) = edited {
                if sr != out_sr {
                    for ch in chans.iter_mut() {
                        *ch = crate::wave::resample_linear(ch, sr, out_sr);
                    }
                }
                let mut waveform = Vec::new();
                let mono = super::WavesPreviewer::mixdown_channels(
                    &chans,
                    chans.get(0).map(|c| c.len()).unwrap_or(0),
                );
                crate::wave::build_minmax(&mut waveform, &mono, 2048);
                self.edited_cache.insert(
                    tab_path.clone(),
                    super::types::CachedEdit {
                        ch_samples: chans,
                        samples_len: mono.len(),
                        waveform_minmax: waveform,
                        dirty: tab.dirty,
                        loop_region: tab.loop_region.map(|v| (v[0], v[1])),
                        loop_region_committed: tab.loop_region.map(|v| (v[0], v[1])),
                        loop_region_applied: tab.loop_region.map(|v| (v[0], v[1])),
                        loop_markers_saved: tab.loop_region.map(|v| (v[0], v[1])),
                        loop_markers_dirty: tab.loop_markers_dirty,
                        markers: tab
                            .markers
                            .iter()
                            .map(project_marker_to_entry)
                            .collect(),
                        markers_committed: tab
                            .markers
                            .iter()
                            .map(project_marker_to_entry)
                            .collect(),
                        markers_applied: tab
                            .markers
                            .iter()
                            .map(project_marker_to_entry)
                            .collect(),
                        markers_saved: tab
                            .markers
                            .iter()
                            .map(project_marker_to_entry)
                            .collect(),
                        markers_dirty: tab.markers_dirty,
                        trim_range: tab.trim_range.map(|v| (v[0], v[1])),
                        loop_xfade_samples: tab.loop_xfade_samples,
                        loop_xfade_shape: loop_shape_from_str(&tab.loop_xfade_shape),
                        fade_in_range: tab.fade_in_range.map(|v| (v[0], v[1])),
                        fade_out_range: tab.fade_out_range.map(|v| (v[0], v[1])),
                        fade_in_shape: fade_shape_from_str(&tab.fade_in_shape),
                        fade_out_shape: fade_shape_from_str(&tab.fade_out_shape),
                        loop_mode: loop_mode_from_str(&tab.loop_mode),
                        snap_zero_cross: tab.snap_zero_cross,
                        tool_state: project_tool_state_to_tool_state(&tab.tool_state),
                        active_tool: tool_kind_from_str(&tab.active_tool),
                        show_waveform_overlay: tab.show_waveform_overlay,
                        bpm_enabled: tab.bpm_enabled,
                        bpm_value: tab.bpm_value,
                        bpm_user_set: tab.bpm_user_set,
                    },
                );
            }
            if !tab_path.is_file() {
                if let Some(item) = self.item_for_path_mut(&tab_path) {
                    item.source = MediaSource::Virtual;
                    item.status =
                        super::types::MediaStatus::DecodeFailed(describe_missing(&tab_path));
                    item.meta = Some(missing_file_meta(&tab_path));
                    if item.virtual_audio.is_none() {
                        item.virtual_audio = Some(std::sync::Arc::new(AudioBuffer::from_channels(
                            vec![Vec::new()],
                        )));
                    }
                }
            }
        }

        for tab in project.tabs.iter() {
            let tab_path = resolve_path(&tab.path, &base_dir);
            self.open_or_activate_tab(&tab_path);
            if let Some(idx) = self.tabs.iter().position(|t| t.path == tab_path) {
                let mut preview_overlay = None;
                let mut preview_tool = None;
                if let Some(raw) = tab.preview_audio.as_ref() {
                    if let Ok((mut chans, sr, _)) = load_sidecar_audio(&project_path, raw) {
                        if sr != out_sr {
                            for ch in chans.iter_mut() {
                                *ch = crate::wave::resample_linear(ch, sr, out_sr);
                            }
                        }
                        let timeline_len = chans.get(0).map(|c| c.len()).unwrap_or_default();
                        let tool = tab
                            .preview_tool
                            .as_deref()
                            .map(tool_kind_from_str)
                            .unwrap_or(super::types::ToolKind::LoopEdit);
                        preview_overlay = Some(super::WavesPreviewer::preview_overlay_from_channels(
                            chans,
                            tool,
                            timeline_len,
                        ));
                        preview_tool = Some(tool);
                    }
                }
                if let Some(t) = self.tabs.get_mut(idx) {
                    t.view_mode = view_mode_from_str(&tab.view_mode);
                    t.show_waveform_overlay = tab.show_waveform_overlay;
                    t.channel_view = project_channel_view_to_channel_view(&tab.channel_view);
                    t.active_tool = tool_kind_from_str(&tab.active_tool);
                    t.tool_state = project_tool_state_to_tool_state(&tab.tool_state);
                    t.loop_mode = loop_mode_from_str(&tab.loop_mode);
                    t.loop_region = tab.loop_region.map(|v| (v[0], v[1]));
                    t.loop_xfade_samples = tab.loop_xfade_samples;
                    t.loop_xfade_shape = loop_shape_from_str(&tab.loop_xfade_shape);
                    t.trim_range = tab.trim_range.map(|v| (v[0], v[1]));
                    t.selection = tab.selection.map(|v| (v[0], v[1]));
                    t.markers = tab.markers.iter().map(project_marker_to_entry).collect();
                    t.markers_saved = t.markers.clone();
                    t.markers_dirty = tab.markers_dirty;
                    t.loop_markers_saved = t.loop_region;
                    t.loop_markers_dirty = tab.loop_markers_dirty;
                    t.fade_in_range = tab.fade_in_range.map(|v| (v[0], v[1]));
                    t.fade_out_range = tab.fade_out_range.map(|v| (v[0], v[1]));
                    t.fade_in_shape = fade_shape_from_str(&tab.fade_in_shape);
                    t.fade_out_shape = fade_shape_from_str(&tab.fade_out_shape);
                    t.snap_zero_cross = tab.snap_zero_cross;
                    t.bpm_enabled = tab.bpm_enabled;
                    t.bpm_value = tab.bpm_value;
                    t.bpm_user_set = tab.bpm_user_set;
                    t.view_offset = tab.view_offset;
                    t.samples_per_px = tab.samples_per_px;
                    t.dirty = tab.dirty;
                    if let Some(overlay) = preview_overlay {
                        t.preview_overlay = Some(overlay);
                        t.preview_audio_tool = preview_tool;
                    }
                }
            }
        }

        if let Some(active) = project.active_tab {
            if active < self.tabs.len() {
                self.active_tab = Some(active);
            }
        }
        if let Some(active) = self.active_tab {
            let (tool, mono) = {
                let Some(tab) = self.tabs.get(active) else {
                    return Ok(());
                };
                let Some(tool) = tab.preview_audio_tool else {
                    return Ok(());
                };
                let Some(overlay) = tab.preview_overlay.as_ref() else {
                    return Ok(());
                };
                let mono = if let Some(m) = overlay.mixdown.as_ref() {
                    m.clone()
                } else {
                    overlay
                        .channels
                        .get(0)
                        .cloned()
                        .unwrap_or_default()
                };
                (tool, mono)
            };
            self.set_preview_mono(active, tool, mono);
        }
        Ok(())
    }

    pub(super) fn process_ipc_requests(&mut self) {
        let Some(rx) = &self.ipc_rx else {
            return;
        };
        let mut pending: Vec<ipc::IpcRequest> = Vec::new();
        {
            let Ok(rx) = rx.lock() else {
                return;
            };
            while let Ok(req) = rx.try_recv() {
                pending.push(req);
            }
        }
        for mut req in pending {
            if let Some(project) = req.project {
                self.queue_project_open(project);
                continue;
            }
            if let Some(pos) = req.files.iter().position(|p| Self::is_session_path(p)) {
                let session = req.files.remove(pos);
                self.queue_project_open(session);
                continue;
            }
            if !req.files.is_empty() {
                let added = self.add_files_merge(&req.files);
                if added > 0 {
                    self.after_add_refresh();
                }
            }
        }
    }

    pub(super) fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<egui::DroppedFile> = ctx.input(|i| i.raw.dropped_files.clone());
        if dropped.is_empty() {
            return;
        }
        let mut project_path: Option<PathBuf> = None;
        let mut external_path: Option<PathBuf> = None;
        let mut paths: Vec<PathBuf> = Vec::new();
        for f in dropped {
            if let Some(p) = f.path {
                let is_project = Self::is_session_path(&p);
                let is_external = p
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| {
                        let s = s.to_ascii_lowercase();
                        s == "csv" || s == "xlsx" || s == "xls"
                    })
                    .unwrap_or(false);
                if is_project && project_path.is_none() {
                    project_path = Some(p);
                } else if is_external && external_path.is_none() {
                    external_path = Some(p);
                } else if !is_project {
                    paths.push(p);
                }
            }
        }
        if let Some(project) = project_path {
            self.queue_project_open(project);
        } else {
            if let Some(data_path) = external_path {
                self.external_sheet_selected = None;
                self.external_sheet_names.clear();
                self.external_settings_dirty = false;
                self.external_load_queue.clear();
                self.external_load_target = Some(external_ops::ExternalLoadTarget::New);
                self.show_external_dialog = true;
                self.begin_external_load(data_path);
            }
            if !paths.is_empty() {
                let added = self.add_files_merge(&paths);
                if added > 0 {
                    self.after_add_refresh();
                }
            }
        }
    }
}
