use std::collections::HashSet;
use std::path::PathBuf;

use super::types::{ClipboardItem, ClipboardPayload, MediaSource, VirtualSourceRef, VirtualState};

impl super::WavesPreviewer {
    #[cfg(windows)]
    fn set_clipboard_files(&self, paths: &[PathBuf]) -> Result<(), String> {
        use clipboard_win::formats::FileList;
        use clipboard_win::{Clipboard, Setter};
        let list: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        let _clip = Clipboard::new_attempts(10).map_err(|e| e.to_string())?;
        FileList.write_clipboard(&list).map_err(|e| e.to_string())
    }

    #[cfg(windows)]
    fn set_clipboard_files_with_marker(
        &self,
        paths: &[PathBuf],
        marker: &str,
    ) -> Result<(), String> {
        // NOTE: egui-winit emits Event::Paste only when clipboard has non-empty text.
        // We add a small marker text alongside the file list so Ctrl+V always produces
        // Event::Paste (otherwise Ctrl+V can "vanish" when clipboard holds only files).
        if marker.is_empty() {
            return self.set_clipboard_files(paths);
        }
        use clipboard_win::formats::{FileList, CF_UNICODETEXT};
        use clipboard_win::{raw, Clipboard, Setter};
        let list: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        let _clip = Clipboard::new_attempts(10).map_err(|e| e.to_string())?;
        FileList.write_clipboard(&list).map_err(|e| e.to_string())?;
        if !marker.is_empty() {
            let mut utf16: Vec<u16> = marker.encode_utf16().collect();
            utf16.push(0);
            let bytes =
                unsafe { std::slice::from_raw_parts(utf16.as_ptr() as *const u8, utf16.len() * 2) };
            raw::set_without_clear(CF_UNICODETEXT, bytes).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    #[cfg(windows)]
    fn set_clipboard_marker_text(&self, marker: &str) -> Result<(), String> {
        // NOTE: keep clipboard text non-empty so Ctrl+V reliably becomes Event::Paste.
        use clipboard_win::formats::Unicode;
        use clipboard_win::{Clipboard, Setter};
        let _clip = Clipboard::new_attempts(10).map_err(|e| e.to_string())?;
        Unicode.write_clipboard(&marker).map_err(|e| e.to_string())
    }

    #[cfg(not(windows))]
    fn set_clipboard_files(&self, _paths: &[PathBuf]) -> Result<(), String> {
        Err("Clipboard file list is not supported on this platform".to_string())
    }

    #[cfg(windows)]
    pub(super) fn get_clipboard_files(&self) -> Vec<PathBuf> {
        use clipboard_win::formats::FileList;
        let list: Vec<String> = clipboard_win::get_clipboard(FileList).unwrap_or_default();
        list.into_iter().map(PathBuf::from).collect()
    }

    #[cfg(not(windows))]
    pub(super) fn get_clipboard_files(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    pub(super) fn copy_selected_to_clipboard(&mut self) {
        const CLIPBOARD_MARKER: &str = "neowaves://clipboard";
        let ids = self.selected_item_ids();
        if ids.is_empty() {
            return;
        }
        self.clear_clipboard_temp_files();
        let out_sr = self.audio.shared.out_sample_rate;
        let mut payload_items: Vec<ClipboardItem> = Vec::new();
        let mut os_paths: Vec<PathBuf> = Vec::new();
        for id in ids {
            let Some(item) = self.item_for_id(id) else {
                continue;
            };
            let display_name = item.display_name.clone();
            let source_path = if item.source == MediaSource::File {
                Some(item.path.clone())
            } else {
                None
            };
            let edited_audio = self.edited_audio_for_path(&item.path);
            let (mut audio, mut sample_rate, mut bits_per_sample) =
                if let Some(audio) = edited_audio.clone() {
                    (Some(audio), out_sr, 32)
                } else {
                    let meta = item.meta.as_ref();
                    (
                        None,
                        meta.map(|m| m.sample_rate).unwrap_or(0),
                        meta.map(|m| m.bits_per_sample).unwrap_or(0),
                    )
                };
            if audio.is_none() {
                if let Some(path) = source_path.as_ref() {
                    if let Some((decoded, sr, bits)) = self.decode_audio_for_virtual(path) {
                        audio = Some(decoded);
                        sample_rate = sr;
                        bits_per_sample = bits;
                    } else if self.debug.cfg.enabled {
                        self.debug_trace_input(format!(
                            "copy_selected_to_clipboard decode failed: {}",
                            path.display()
                        ));
                    }
                }
            }
            if let Some(audio_ref) = edited_audio {
                if audio_ref.len() > 0 {
                    if let Some(tmp) =
                        self.export_audio_to_temp_wav(&display_name, &audio_ref, out_sr)
                    {
                        os_paths.push(tmp);
                    }
                }
            } else if let Some(path) = &source_path {
                if path.is_file() {
                    os_paths.push(path.clone());
                }
            }
            payload_items.push(ClipboardItem {
                display_name,
                source_path,
                audio,
                sample_rate,
                bits_per_sample,
            });
        }
        self.clipboard_payload = Some(ClipboardPayload {
            items: payload_items,
            created_at: std::time::Instant::now(),
        });
        if self.debug.cfg.enabled {
            let count = self
                .clipboard_payload
                .as_ref()
                .map(|p| p.items.len())
                .unwrap_or(0);
            self.debug.last_copy_at = Some(std::time::Instant::now());
            self.debug.last_copy_count = count;
            self.debug_trace_input(format!("copy_selected_to_clipboard items={count}"));
        }
        if !os_paths.is_empty() {
            #[cfg(windows)]
            {
                if let Err(err) = self.set_clipboard_files_with_marker(&os_paths, CLIPBOARD_MARKER)
                {
                    self.debug_log(format!("clipboard error: {err}"));
                }
            }
            #[cfg(not(windows))]
            {
                if let Err(err) = self.set_clipboard_files(&os_paths) {
                    self.debug_log(format!("clipboard error: {err}"));
                }
            }
        } else {
            #[cfg(windows)]
            {
                if let Err(err) = self.set_clipboard_marker_text(CLIPBOARD_MARKER) {
                    self.debug_log(format!("clipboard error: {err}"));
                }
            }
        }
    }

    pub(super) fn paste_clipboard_to_list(&mut self) {
        let before = self.capture_list_selection_snapshot();
        let payload = self.clipboard_payload.clone();
        let mut added_any = false;
        let mut added_paths: Vec<PathBuf> = Vec::new();
        if self.debug.cfg.enabled {
            let payload_items = payload.as_ref().map(|p| p.items.len()).unwrap_or(0);
            self.debug_trace_input(format!(
                "paste start payload_items={} selected={:?} selected_multi={}",
                payload_items,
                self.selected,
                self.selected_multi.len()
            ));
        }
        if let Some(payload) = payload {
            let mut insert_idx = self.items.len();
            if let Some(row) = self
                .selected_multi
                .iter()
                .next_back()
                .copied()
                .or(self.selected)
            {
                if let Some(id) = self.files.get(row) {
                    if let Some(item_idx) = self.item_index.get(id) {
                        insert_idx = (*item_idx + 1).min(self.items.len());
                    }
                }
            }
            if self.debug.cfg.enabled {
                self.debug_trace_input(format!("paste insert_idx={insert_idx}"));
            }
            let mut skipped_missing_audio = 0usize;
            let mut decoded_from_source = 0usize;
            for item in payload.items {
                let mut audio = item.audio.clone();
                let mut sample_rate = item.sample_rate;
                let mut bits_per_sample = item.bits_per_sample;
                if audio.is_none() {
                    if let Some(path) = item.source_path.as_ref() {
                        if let Some((decoded, sr, bits)) = self.decode_audio_for_virtual(path) {
                            audio = Some(decoded);
                            sample_rate = sr;
                            bits_per_sample = bits;
                            decoded_from_source += 1;
                        } else if self.debug.cfg.enabled {
                            self.debug_trace_input(format!(
                                "paste decode failed: {}",
                                path.display()
                            ));
                        }
                    }
                }
                let Some(audio) = audio else {
                    skipped_missing_audio += 1;
                    continue;
                };
                let name = self.unique_virtual_display_name(&item.display_name);
                let virtual_state = Some(VirtualState {
                    source: VirtualSourceRef::Sidecar("clipboard".to_string()),
                    op_chain: Vec::new(),
                    sample_rate: sample_rate.max(1),
                    channels: audio.channels.len().max(1) as u16,
                    bits_per_sample,
                });
                let vitem = self.make_virtual_item(
                    name,
                    audio,
                    sample_rate,
                    bits_per_sample,
                    virtual_state,
                );
                added_paths.push(vitem.path.clone());
                self.add_virtual_item(vitem, Some(insert_idx));
                insert_idx = insert_idx.saturating_add(1);
                added_any = true;
            }
            if added_any {
                self.after_add_refresh();
                self.selected_multi.clear();
                for p in &added_paths {
                    if let Some(row) = self.row_for_path(p) {
                        self.selected_multi.insert(row);
                    }
                }
                self.selected = self.selected_multi.iter().next().copied();
                self.record_list_insert_from_paths(&added_paths, before);
                if self.debug.cfg.enabled {
                    self.debug.last_paste_at = Some(std::time::Instant::now());
                    self.debug.last_paste_count = added_paths.len();
                    self.debug.last_paste_source = Some("internal".to_string());
                    self.debug_trace_input(format!(
                        "paste_clipboard_to_list internal items={}",
                        added_paths.len()
                    ));
                    self.debug_trace_input(format!(
                        "paste internal decoded_from_source={} skipped_missing_audio={}",
                        decoded_from_source, skipped_missing_audio
                    ));
                }
                return;
            }
            if self.debug.cfg.enabled {
                self.debug_trace_input("paste_clipboard_to_list internal empty");
                self.debug_trace_input(format!(
                    "paste internal decoded_from_source={} skipped_missing_audio={}",
                    decoded_from_source, skipped_missing_audio
                ));
            }
        }
        let existing_paths: HashSet<PathBuf> =
            self.items.iter().map(|item| item.path.clone()).collect();
        let files = self.get_clipboard_files();
        if self.debug.cfg.enabled {
            self.debug_trace_input(format!("paste os files={}", files.len()));
        }
        if !files.is_empty() {
            let added = self.add_files_merge(&files);
            if added > 0 {
                self.after_add_refresh();
                let new_paths: Vec<PathBuf> = self
                    .items
                    .iter()
                    .filter(|item| !existing_paths.contains(&item.path))
                    .map(|item| item.path.clone())
                    .collect();
                self.record_list_insert_from_paths(&new_paths, before);
                if self.debug.cfg.enabled {
                    self.debug.last_paste_at = Some(std::time::Instant::now());
                    self.debug.last_paste_count = added;
                    self.debug.last_paste_source = Some("os".to_string());
                    self.debug_trace_input(format!("paste_clipboard_to_list os files={added}"));
                }
            }
        } else if self.debug.cfg.enabled {
            self.debug_trace_input("paste_clipboard_to_list os empty");
        }
    }

    pub(super) fn handle_clipboard_hotkeys(&mut self, ctx: &egui::Context) {
        if self.active_tab.is_some() {
            return;
        }
        let search_focused = ctx.memory(|m| m.has_focus(Self::search_box_id()));
        let list_focus = self.list_has_focus || ctx.memory(|m| m.has_focus(Self::list_focus_id()));
        let allow = !search_focused
            && (list_focus || self.selected.is_some() || !self.selected_multi.is_empty());
        let ctrl = ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
        let down_c = ctx.input(|i| i.key_down(egui::Key::C));
        let down_v = ctx.input(|i| i.key_down(egui::Key::V));
        let mut event_copy = false;
        let mut event_paste = false;
        // NOTE: egui-winit intercepts Ctrl+C/V and turns them into Event::Copy / Event::Paste,
        // so Key::C/Key::V may never appear. We must consume from `i.events` here.
        ctx.input(|i| {
            for ev in &i.events {
                match ev {
                    egui::Event::Copy => event_copy = true,
                    egui::Event::Paste(_) => event_paste = true,
                    _ => {}
                }
            }
        });
        let mut consumed_copy_event = false;
        let mut consumed_paste_event = false;
        let mut paste_text: Option<String> = None; // Some(text) only if OS clipboard provides non-empty text.
        if allow {
            ctx.input_mut(|i| {
                let mut idx = 0;
                while idx < i.events.len() {
                    match &i.events[idx] {
                        egui::Event::Copy => {
                            consumed_copy_event = true;
                            i.events.remove(idx);
                            continue;
                        }
                        egui::Event::Paste(s) => {
                            consumed_paste_event = true;
                            paste_text = Some(s.clone());
                            i.events.remove(idx);
                            continue;
                        }
                        _ => {}
                    }
                    idx += 1;
                }
            });
        }
        let edge_c = allow && ctrl && down_c && !self.clipboard_c_was_down;
        let edge_v = allow && ctrl && down_v && !self.clipboard_v_was_down;
        let consumed_copy =
            allow && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::C));
        let consumed_paste =
            allow && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::V));
        let copy_trigger = consumed_copy_event || consumed_copy || edge_c;
        let paste_trigger = consumed_paste_event || consumed_paste || edge_v;
        let copy_source = if consumed_copy_event {
            "Event::Copy"
        } else if consumed_copy {
            "consume_key"
        } else if edge_c {
            "edge"
        } else {
            "none"
        };
        let paste_source = if consumed_paste_event {
            "Event::Paste"
        } else if consumed_paste {
            "consume_key"
        } else if edge_v {
            "edge"
        } else {
            "none"
        };
        self.clipboard_c_was_down = down_c;
        self.clipboard_v_was_down = down_v;
        if self.debug.cfg.enabled {
            self.debug.last_clip_allow = allow;
            self.debug.last_clip_wants_kb = ctx.wants_keyboard_input();
            self.debug.last_clip_ctrl = ctrl;
            self.debug.last_clip_event_copy = event_copy;
            self.debug.last_clip_event_paste = event_paste;
            self.debug.last_clip_raw_key_c = down_c;
            self.debug.last_clip_raw_key_v = down_v;
            self.debug.last_clip_os_ctrl = ctrl;
            self.debug.last_clip_os_key_c = edge_c;
            self.debug.last_clip_os_key_v = edge_v;
            self.debug.last_clip_consumed_copy = consumed_copy_event || consumed_copy;
            self.debug.last_clip_consumed_paste = consumed_paste_event || consumed_paste;
            self.debug.last_clip_copy_trigger = copy_trigger;
            self.debug.last_clip_paste_trigger = paste_trigger;
            if allow && ctrl && !paste_trigger && self.clipboard_payload.is_some() {
                self.debug_trace_input(format!(
                    "paste not triggered despite allow: down_v={} list_focus={} search_focus={} sel={:?}",
                    down_v, list_focus, search_focused, self.selected
                ));
            }
            if !allow && ctrl && (down_c || down_v) {
                self.debug_trace_input(format!(
                    "clipboard blocked (list_focus={} search_focus={})",
                    list_focus, search_focused
                ));
            }
        }
        if copy_trigger {
            if !self.selected_multi.is_empty() || self.selected.is_some() {
                self.copy_selected_to_clipboard();
                if self.debug.cfg.enabled {
                    self.debug_trace_input(format!("copy triggered via {copy_source}"));
                }
            } else if self.debug.cfg.enabled {
                self.debug_trace_input("copy triggered with no selection");
            }
        }
        if paste_trigger {
            self.paste_clipboard_to_list();
            if self.debug.cfg.enabled {
                if let Some(text) = paste_text.as_deref() {
                    self.debug_trace_input(format!(
                        "paste triggered via {paste_source} text_len={}",
                        text.len()
                    ));
                } else {
                    self.debug_trace_input(format!("paste triggered via {paste_source}"));
                }
            }
        }
    }
}
