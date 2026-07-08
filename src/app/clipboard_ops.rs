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

    #[cfg(windows)]
    fn get_clipboard_text(&self) -> Option<String> {
        use clipboard_win::formats::Unicode;
        clipboard_win::get_clipboard(Unicode).ok()
    }

    #[cfg(not(windows))]
    fn get_clipboard_text(&self) -> Option<String> {
        None
    }

    pub(super) fn copy_selected_to_clipboard(&mut self) {
        let ids = self.selected_item_ids();
        if ids.is_empty() {
            return;
        }
        if self.clipboard_prep_state.is_some() {
            return;
        }
        self.clear_clipboard_temp_files();
        let out_sr = self.audio.shared.out_sample_rate;
        // Snapshot the inputs cheaply on the UI thread; the expensive parts
        // (decoding file-backed items and exporting edited audio to temp
        // WAVs) run on a worker so a large multi-selection cannot freeze
        // the UI. The busy overlay blocks input until the clipboard is set.
        let mut prep_items: Vec<crate::app::types::ClipboardPrepItem> = Vec::new();
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
            let meta_sr = item.meta.as_ref().map(|m| m.sample_rate).unwrap_or(0);
            let meta_bits = item.meta.as_ref().map(|m| m.bits_per_sample).unwrap_or(0);
            let audio = if let Some(audio) = self.edited_audio_for_path(&item.path) {
                crate::app::types::ClipboardPrepAudio::Ready {
                    audio,
                    sample_rate: out_sr,
                    bits_per_sample: 32,
                }
            } else {
                crate::app::types::ClipboardPrepAudio::DecodeFromFile {
                    sample_rate: meta_sr,
                    bits_per_sample: meta_bits,
                }
            };
            // Pending gain/sample-rate overrides are list-level and independent
            // of any in-memory edited audio above; apply them the same way
            // native_drag does so copy and drag-export never diverge.
            let gain_db = self.pending_gain_db_for_path(&item.path);
            let target_sample_rate = self
                .sample_rate_override
                .get(&item.path)
                .copied()
                .filter(|sr| *sr > 0);
            let resample_quality = Self::to_wave_resample_quality(self.src_quality);
            prep_items.push(crate::app::types::ClipboardPrepItem {
                display_name,
                source_path,
                audio,
                gain_db,
                target_sample_rate,
                resample_quality,
            });
        }
        if prep_items.is_empty() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            crate::app::threading::lower_current_thread_priority();
            let done = Self::run_clipboard_prep(prep_items);
            let _ = tx.send(done);
        });
        self.clipboard_prep_state = Some(crate::app::types::ClipboardPrepState {
            rx,
            started_at: std::time::Instant::now(),
        });
        if self.debug.cfg.enabled {
            self.debug_trace_input("copy_selected_to_clipboard queued".to_string());
        }
    }

    /// Worker half of the clipboard copy: decode file-backed items for the
    /// in-app payload and export edited audio to temp WAVs for the OS
    /// clipboard file list.
    fn run_clipboard_prep(
        prep_items: Vec<crate::app::types::ClipboardPrepItem>,
    ) -> crate::app::types::ClipboardPrepDone {
        use crate::app::types::{ClipboardPrepAudio, ClipboardPrepDone};
        let mut payload_items: Vec<ClipboardItem> = Vec::new();
        let mut os_paths: Vec<PathBuf> = Vec::new();
        let mut temp_files: Vec<PathBuf> = Vec::new();
        for item in prep_items {
            let gain_db = item.gain_db;
            let target_sample_rate = item.target_sample_rate;
            let resample_quality = item.resample_quality;
            let was_ready = matches!(item.audio, ClipboardPrepAudio::Ready { .. });
            let (mut audio, mut sample_rate, bits_per_sample) = match item.audio {
                ClipboardPrepAudio::Ready {
                    audio,
                    sample_rate,
                    bits_per_sample,
                    ..
                } => (Some(audio), sample_rate, bits_per_sample),
                ClipboardPrepAudio::DecodeFromFile {
                    sample_rate,
                    bits_per_sample,
                } => {
                    let mut sr = sample_rate;
                    let mut bits = bits_per_sample;
                    let mut audio = None;
                    if let Some(path) = item.source_path.as_ref() {
                        if let Ok((chans, in_sr)) = crate::audio_io::decode_audio_multi(path) {
                            bits = crate::audio_io::read_audio_info(path)
                                .map(|info| info.bits_per_sample)
                                .unwrap_or(32);
                            sr = in_sr.max(1);
                            audio = Some(std::sync::Arc::new(
                                crate::audio::AudioBuffer::from_channels(chans),
                            ));
                        }
                    }
                    (audio, sr, bits)
                }
            };

            let target_sr = target_sample_rate.unwrap_or(sample_rate);
            let has_override = gain_db.abs() > 0.0001 || target_sr != sample_rate;
            if has_override {
                if let Some(current) = audio.take() {
                    let (channels, new_sr) = super::WavesPreviewer::apply_gain_and_resample(
                        current.channels.clone(),
                        sample_rate,
                        gain_db,
                        target_sr,
                        resample_quality,
                    );
                    sample_rate = new_sr;
                    audio = Some(std::sync::Arc::new(crate::audio::AudioBuffer::from_channels(
                        channels,
                    )));
                }
            }

            // Only re-export a temp WAV when the audio differs from the
            // original file bytes (edited/virtual audio, or an override was
            // applied); otherwise reference the original file, same as before.
            let needs_temp_export = was_ready || has_override;
            if needs_temp_export {
                if let Some(audio_ref) = audio.as_ref().filter(|a| a.len() > 0) {
                    if let Some(tmp) =
                        crate::app::temp_audio_ops::allocate_neowaves_temp_cache_path(
                            "clipboard", "wav",
                        )
                    {
                        let range = (0, audio_ref.len());
                        if crate::wave::export_selection_wav(
                            &audio_ref.channels,
                            sample_rate,
                            range,
                            &tmp,
                        )
                        .is_ok()
                        {
                            os_paths.push(tmp.clone());
                            temp_files.push(tmp);
                        }
                    }
                }
            } else if let Some(path) = item.source_path.as_ref() {
                if path.is_file() {
                    os_paths.push(path.clone());
                }
            }

            payload_items.push(ClipboardItem {
                display_name: item.display_name,
                source_path: item.source_path,
                audio,
                sample_rate,
                bits_per_sample,
            });
        }
        ClipboardPrepDone {
            payload: ClipboardPayload {
                items: payload_items,
                created_at: std::time::Instant::now(),
            },
            os_paths,
            temp_files,
        }
    }

    pub(super) fn drain_clipboard_prep(&mut self, ctx: &egui::Context) {
        #[cfg(windows)]
        const CLIPBOARD_MARKER: &str = "neowaves://clipboard";
        let done = match &self.clipboard_prep_state {
            Some(state) => match state.rx.try_recv() {
                Ok(done) => Some(done),
                Err(_) => None,
            },
            None => None,
        };
        let Some(done) = done else {
            return;
        };
        self.clipboard_prep_state = None;
        self.clipboard_temp_files.extend(done.temp_files);
        let count = done.payload.items.len();
        let os_paths = done.os_paths;
        self.clipboard_payload = Some(done.payload);
        if self.debug.cfg.enabled {
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
        ctx.request_repaint();
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

    fn handle_effect_graph_clipboard_hotkeys(&mut self, ctx: &egui::Context) {
        if !self.is_effect_graph_workspace_active() {
            return;
        }
        let allow = !ctx.egui_wants_keyboard_input();
        let ctrl = ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
        let down_c = ctx.input(|i| i.key_down(egui::Key::C));
        let down_v = ctx.input(|i| i.key_down(egui::Key::V));
        let mut consumed_copy_event = false;
        let mut consumed_paste_event = false;
        let mut paste_text: Option<String> = None;
        if allow {
            ctx.input_mut(|i| {
                let mut idx = 0usize;
                while idx < i.events.len() {
                    match &i.events[idx] {
                        egui::Event::Copy => {
                            consumed_copy_event = true;
                            i.events.remove(idx);
                            continue;
                        }
                        egui::Event::Paste(text)
                            if self.effect_graph_clipboard_text_is_supported(text) =>
                        {
                            consumed_paste_event = true;
                            paste_text = Some(text.clone());
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
        self.clipboard_c_was_down = down_c;
        self.clipboard_v_was_down = down_v;

        if (consumed_copy_event || consumed_copy || edge_c)
            && self.effect_graph_can_copy_selection()
        {
            let _ = self.effect_graph_copy_selection_to_clipboard(ctx);
        }
        if consumed_paste_event || consumed_paste || edge_v {
            let clipboard_text = paste_text
                .or_else(|| self.get_clipboard_text())
                .filter(|text| self.effect_graph_clipboard_text_is_supported(text));
            if let Some(text) = clipboard_text {
                if let Err(err) = self.effect_graph_paste_from_clipboard_text(&text) {
                    self.push_effect_graph_console(
                        super::types::EffectGraphSeverity::Error,
                        "clipboard",
                        format!("paste failed: {err}"),
                        None,
                    );
                }
            }
        }
    }

    pub(super) fn handle_clipboard_hotkeys(&mut self, ctx: &egui::Context) {
        if self.is_effect_graph_workspace_active() {
            self.handle_effect_graph_clipboard_hotkeys(ctx);
            return;
        }
        if !self.is_list_workspace_active() {
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
            self.debug.last_clip_wants_kb = ctx.egui_wants_keyboard_input();
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

#[cfg(test)]
mod prep_tests {
    use super::super::types::{ClipboardPrepAudio, ClipboardPrepItem};
    use crate::wave::ResampleQuality;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "neowaves_clipboard_prep_test_{tag}_{}_{}",
            std::process::id(),
            ts
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn pending_gain_override_materializes_and_applies_gain() {
        let dir = temp_dir("gain");
        let wav = dir.join("source.wav");
        crate::wave::export_channels_audio(&[vec![0.2, 0.2, 0.2]], 48_000, &wav)
            .expect("write wav");
        let item = ClipboardPrepItem {
            display_name: "source.wav".to_string(),
            source_path: Some(wav.clone()),
            audio: ClipboardPrepAudio::DecodeFromFile {
                sample_rate: 48_000,
                bits_per_sample: 32,
            },
            gain_db: -6.0,
            target_sample_rate: None,
            resample_quality: ResampleQuality::Fast,
        };

        let done = crate::WavesPreviewer::run_clipboard_prep(vec![item]);

        assert_eq!(done.payload.items.len(), 1);
        assert_eq!(done.os_paths.len(), 1);
        assert_ne!(
            done.os_paths[0],
            std::fs::canonicalize(&wav).unwrap(),
            "gain override must materialize a new file, not reference the original"
        );
        assert_eq!(done.temp_files.len(), 1);
        let audio = done.payload.items[0]
            .audio
            .as_ref()
            .expect("decoded audio present");
        let expected = 0.2 * crate::app::helpers::db_to_amp(-6.0);
        assert!(
            (audio.channels[0][0] - expected).abs() < 1e-4,
            "expected gain applied: got {} want ~{}",
            audio.channels[0][0],
            expected
        );
    }

    #[test]
    fn no_override_references_original_file() {
        let dir = temp_dir("plain");
        let wav = dir.join("source.wav");
        crate::wave::export_channels_audio(&[vec![0.1, 0.1, 0.1]], 48_000, &wav)
            .expect("write wav");
        let item = ClipboardPrepItem {
            display_name: "source.wav".to_string(),
            source_path: Some(wav.clone()),
            audio: ClipboardPrepAudio::DecodeFromFile {
                sample_rate: 48_000,
                bits_per_sample: 32,
            },
            gain_db: 0.0,
            target_sample_rate: None,
            resample_quality: ResampleQuality::Fast,
        };

        let done = crate::WavesPreviewer::run_clipboard_prep(vec![item]);

        assert_eq!(done.os_paths, vec![wav]);
        assert!(done.temp_files.is_empty());
    }
}

#[cfg(all(test, windows))]
mod tests {
    // Verify clipboard-win 5.x API surface compiles and exports are accessible.
    // Actual clipboard read/write tests require a running GUI message loop and
    // would modify shared system state, so they are omitted from automated runs.

    #[test]
    fn clipboard_win_raw_module_accessible() {
        // raw::set_without_clear must exist in clipboard-win 5.x
        let _fn_exists: fn(u32, &[u8]) -> clipboard_win::SysResult<()> =
            clipboard_win::raw::set_without_clear;
        let _ = _fn_exists; // suppress unused warning
    }

    #[test]
    fn clipboard_win_formats_accessible() {
        use clipboard_win::formats::{CF_UNICODETEXT, FileList, Unicode};
        // Just verify these types are importable and constructable
        let _ = FileList;
        let _ = Unicode;
        let _cf: u32 = CF_UNICODETEXT;
    }

    #[test]
    fn clipboard_win_clipboard_new_attempts_api_exists() {
        // Verify Clipboard::new_attempts exists with the expected signature
        // (does not actually open the clipboard in this test)
        let _ = clipboard_win::Clipboard::new_attempts as fn(usize) -> clipboard_win::SysResult<clipboard_win::Clipboard>;
    }

    #[test]
    fn clipboard_win_get_clipboard_unicode_api_exists() {
        use clipboard_win::formats::Unicode;
        // Verify get_clipboard(Unicode) signature is callable
        let _ = clipboard_win::get_clipboard as fn(Unicode) -> clipboard_win::SysResult<String>;
    }

    #[test]
    fn utf16_encode_round_trip_for_marker() {
        // Verify the UTF-16 encoding logic used in set_clipboard_files_with_marker
        let marker = "neowaves_paste\0";
        let mut utf16: Vec<u16> = marker.encode_utf16().collect();
        utf16.push(0);
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(utf16.as_ptr() as *const u8, utf16.len() * 2)
        };
        // Decode back and verify
        let decoded: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|&c| c != 0)
            .collect();
        let result = String::from_utf16_lossy(&decoded);
        assert!(result.starts_with("neowaves_paste"), "round-trip: {result}");
    }
}
