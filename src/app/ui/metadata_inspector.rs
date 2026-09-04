use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Color32, RichText, Stroke};

use crate::app::types::{
    EditorPrimaryView, MetadataActionResult, MetadataDetailTab, MetadataHexPage, MetadataSubView,
    VirtualSourceRef,
};
use crate::metadata::{
    ContentKind, HashAlgorithm, MetadataDocument, MetadataNode, NodeId, PayloadRef, SearchKind,
    SourceRange,
};

const HEX_PAGE_BYTES: usize = 64 * 1024;
const HEX_WAVEFORM_RAIL_WIDTH: f32 = 220.0;

fn metadata_surface_fill() -> Color32 {
    Color32::from_rgb(18, 23, 30)
}

fn metadata_surface_stroke() -> Color32 {
    Color32::from_rgb(48, 60, 74)
}

fn metadata_accent() -> Color32 {
    Color32::from_rgb(75, 204, 176)
}

fn metadata_value_text() -> Color32 {
    Color32::from_rgb(226, 232, 239)
}

enum PendingMetadataAction {
    Search {
        payload: PayloadRef,
        kind: SearchKind,
        query: String,
    },
    Hash {
        payload: PayloadRef,
    },
    Copy {
        payload: PayloadRef,
    },
    Extract {
        payload: PayloadRef,
        output: PathBuf,
        overwrite: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum MetadataEditorNavigation {
    Sample(u64),
    Range { start: u64, end_inclusive: u64 },
    Tempo(f32),
}

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_metadata_inspector(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        tab_idx: usize,
    ) {
        self.poll_metadata_jobs(tab_idx, ctx);
        let source = self.metadata_source_for_tab(tab_idx);
        if self.tabs[tab_idx].metadata_document.is_none()
            && !self.tabs[tab_idx].metadata_loading
            && self.tabs[tab_idx].metadata_scan_rx.is_none()
        {
            if let Some((path, _)) = source.clone() {
                self.start_metadata_scan(tab_idx, path);
            }
        }

        let mut leave_metadata = None;
        let mut refresh = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("View:");
            egui::ComboBox::from_id_salt(("metadata_primary_view", self.tabs[tab_idx].tab_id))
                .selected_text("Metadata")
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(false, "Wave")
                        .on_hover_text("Return to waveform editor")
                        .clicked()
                    {
                        leave_metadata = Some(EditorPrimaryView::Wave);
                    }
                    if ui.selectable_label(false, "Spec").clicked() {
                        leave_metadata = Some(EditorPrimaryView::Spec);
                    }
                    if ui.selectable_label(false, "Other").clicked() {
                        leave_metadata = Some(EditorPrimaryView::Other);
                    }
                    let _ = ui.selectable_label(true, "Metadata");
                });
            ui.separator();
            ui.selectable_value(
                &mut self.tabs[tab_idx].metadata_sub_view,
                MetadataSubView::Structure,
                "Structure",
            );
            ui.selectable_value(
                &mut self.tabs[tab_idx].metadata_sub_view,
                MetadataSubView::Hex,
                "Hex",
            );
            ui.separator();
            if ui.button("Refresh").clicked() {
                refresh = true;
            }
            if self.tabs[tab_idx].metadata_loading {
                ui.spinner();
                ui.label("Scanning...");
            }
            if let Some(document) = self.tabs[tab_idx].metadata_document.as_ref() {
                ui.label(
                    RichText::new(format!(
                        "{} · {:?} · {} nodes",
                        document.container.key(),
                        document.completion,
                        document.nodes.len()
                    ))
                    .weak(),
                );
            }
        });
        if let Some(primary) = leave_metadata {
            self.tabs[tab_idx].primary_view = primary;
            return;
        }
        if refresh {
            self.reset_metadata_tab(tab_idx);
            if let Some((path, _)) = source.clone() {
                self.start_metadata_scan(tab_idx, path);
            }
        }

        let Some((source_path, source_is_virtual_origin)) = source else {
            ui.separator();
            ui.colored_label(
                Color32::YELLOW,
                "Metadata is unavailable: this virtual item has no readable source file.",
            );
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::Label::new(RichText::new(source_path.display().to_string()).monospace())
                    .truncate()
                    .show_tooltip_when_elided(true),
            );
            if source_is_virtual_origin {
                ui.colored_label(
                    Color32::YELLOW,
                    "Source metadata — it may not match the current edited buffer.",
                );
            }
        });
        if let Some(error) = self.tabs[tab_idx].metadata_error.clone() {
            ui.colored_label(Color32::LIGHT_RED, error);
        }
        ui.separator();

        let Some(document) = self.tabs[tab_idx].metadata_document.clone() else {
            if !self.tabs[tab_idx].metadata_loading {
                ui.label("No metadata document is available.");
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return;
        };
        let mut registry_changed = false;
        let mut detected_additions = Vec::new();
        ui.horizontal_wrapped(|ui| {
            ui.menu_button("Detected Fields", |ui| {
                ui.label("Normalized fields in this file");
                for field in &document.normalized {
                    let key = crate::app::types::ColumnKey::Normalized(field.key.clone());
                    let registered = self
                        .metadata_list_columns
                        .iter()
                        .any(|column| column.key == key && column.visible);
                    if ui
                        .add_enabled(
                            !registered,
                            egui::Button::new(if registered {
                                format!("✓ {}", field.key)
                            } else {
                                format!("+ {}", field.key)
                            }),
                        )
                        .clicked()
                    {
                        detected_additions.push((key, field.key.clone()));
                    }
                }
                if !self.metadata_list_columns.is_empty() {
                    ui.separator();
                    ui.label("Registered columns");
                    for column in &mut self.metadata_list_columns {
                        registry_changed |= ui
                            .checkbox(
                                &mut column.visible,
                                format!("{}  ({})", column.label, column.key.serialized_name()),
                            )
                            .changed();
                    }
                }
            });
        });
        for (key, label) in detected_additions {
            self.add_metadata_list_column(key, label);
        }
        if registry_changed {
            self.save_metadata_column_registry();
            self.ensure_sort_key_visible();
        }
        let exact_live = self.metadata_live_source_range(tab_idx, &document, &source_path);
        let source_time = match &self.playback_session.source {
            crate::app::PlaybackSourceKind::EditorTab(path) if path == &self.tabs[tab_idx].path => {
                self.playback_current_source_time_sec()
            }
            _ => None,
        };
        if self.playback_is_playing_now() {
            ctx.request_repaint();
        }

        match self.tabs[tab_idx].metadata_sub_view {
            MetadataSubView::Structure => self.ui_metadata_structure(
                ui,
                ctx,
                tab_idx,
                &source_path,
                &document,
                exact_live,
                source_time,
            ),
            MetadataSubView::Hex => self.ui_metadata_hex(
                ui,
                ctx,
                tab_idx,
                &source_path,
                &document,
                exact_live,
                source_time,
            ),
        }
    }

    /// The file whose metadata this tab should show.
    ///
    /// Runs every frame the inspector is visible, so it must not stat: on a
    /// share that would be one blocking syscall per frame. `path_status`
    /// answers from cache and probes in the background, and a path it has
    /// not resolved yet counts as present -- the metadata read that follows
    /// is itself async and reports its own failure if the file is gone.
    fn metadata_source_for_tab(&mut self, tab_idx: usize) -> Option<(PathBuf, bool)> {
        let tab_path = self.tabs.get(tab_idx)?.path.clone();
        if !self.is_virtual_path(&tab_path) {
            return self
                .path_is_file_cached(&tab_path)
                .then(|| (tab_path, false));
        }
        let mut current = tab_path;
        for _ in 0..16 {
            let item = self.item_for_path(&current)?;
            let source = item.virtual_state.as_ref().map(|state| &state.source)?;
            match source {
                VirtualSourceRef::FilePath(path) => {
                    let path = path.clone();
                    return self.path_is_file_cached(&path).then_some((path, true));
                }
                VirtualSourceRef::VirtualPath(path) => current = path.clone(),
                VirtualSourceRef::Sidecar(_) => return None,
            }
        }
        None
    }

    fn reset_metadata_tab(&mut self, tab_idx: usize) {
        let tab = &mut self.tabs[tab_idx];
        tab.metadata_document = None;
        tab.metadata_loading = false;
        tab.metadata_error = None;
        tab.metadata_scan_rx = None;
        tab.metadata_selected_node = None;
        tab.metadata_detail_tab = MetadataDetailTab::Properties;
        tab.metadata_text_encoding = 0;
        tab.metadata_hex_selection = None;
        tab.metadata_hex_seek_fraction = None;
        tab.metadata_hex_scroll_target = None;
        tab.metadata_hex_page = None;
        tab.metadata_hex_pages.clear();
        tab.metadata_hex_page_requested = None;
        tab.metadata_hex_page_rx = None;
        tab.metadata_search_results.clear();
        tab.metadata_action_status = None;
        tab.metadata_action_rx = None;
        if let Some(cancel) = tab.metadata_action_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        tab.metadata_action_progress = None;
        tab.metadata_action_total = 0;
        tab.metadata_artwork_requested = None;
        tab.metadata_artwork_rx = None;
        tab.metadata_artwork_texture = None;
    }

    fn start_metadata_scan(&mut self, tab_idx: usize, path: PathBuf) {
        let (tx, rx) = std::sync::mpsc::channel();
        let tab = &mut self.tabs[tab_idx];
        tab.metadata_loading = true;
        tab.metadata_error = None;
        tab.metadata_scan_rx = Some(rx);
        std::thread::spawn(move || {
            let result =
                crate::metadata::inspect_path(&path, crate::metadata::InspectOptions::default())
                    .map_err(|error| error.to_string());
            let _ = tx.send(result);
        });
    }

    fn poll_metadata_jobs(&mut self, tab_idx: usize, ctx: &egui::Context) {
        let scan_result = self.tabs[tab_idx]
            .metadata_scan_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some(result) = scan_result {
            let tab = &mut self.tabs[tab_idx];
            tab.metadata_scan_rx = None;
            tab.metadata_loading = false;
            match result {
                Ok(document) => {
                    tab.metadata_selected_node = document.roots.first().copied();
                    tab.metadata_hex_offset = 0;
                    tab.metadata_document = Some(Arc::new(document));
                }
                Err(error) => tab.metadata_error = Some(error),
            }
            ctx.request_repaint();
        }

        let page_result = self.tabs[tab_idx]
            .metadata_hex_page_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some(result) = page_result {
            let tab = &mut self.tabs[tab_idx];
            tab.metadata_hex_page_rx = None;
            let requested = tab.metadata_hex_page_requested;
            tab.metadata_hex_page_requested = None;
            match result {
                Ok(pages) => {
                    for (start, bytes) in pages {
                        tab.metadata_hex_pages.retain(|page| page.start != start);
                        let page = MetadataHexPage {
                            start,
                            bytes: Arc::new(bytes),
                        };
                        if requested == Some(start) {
                            tab.metadata_hex_page = Some(page.clone());
                        }
                        tab.metadata_hex_pages.push_back(page);
                    }
                    while tab.metadata_hex_pages.len() > 128 {
                        tab.metadata_hex_pages.pop_front();
                    }
                }
                Err(error) => tab.metadata_error = Some(error),
            }
            ctx.request_repaint();
        }

        let action_result = self.tabs[tab_idx]
            .metadata_action_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some(result) = action_result {
            let tab = &mut self.tabs[tab_idx];
            tab.metadata_action_rx = None;
            tab.metadata_action_cancel = None;
            tab.metadata_action_progress = None;
            tab.metadata_action_total = 0;
            match result {
                Ok(MetadataActionResult::Search(offsets)) => {
                    tab.metadata_action_status = Some(format!("{} matches", offsets.len()));
                    tab.metadata_search_results = offsets;
                }
                Ok(MetadataActionResult::Hash(value)) => {
                    tab.metadata_action_status = Some(format!("SHA-256: {value}"));
                }
                Ok(MetadataActionResult::Extracted(path)) => {
                    tab.metadata_action_status = Some(format!("Extracted to {}", path.display()));
                }
                Ok(MetadataActionResult::CopyHex(value)) => {
                    ctx.copy_text(value);
                    tab.metadata_action_status = Some("Copied hex bytes".to_string());
                }
                Err(error) => tab.metadata_action_status = Some(format!("Error: {error}")),
            }
            ctx.request_repaint();
        }

        let artwork_result = self.tabs[tab_idx]
            .metadata_artwork_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some(result) = artwork_result {
            let tab = &mut self.tabs[tab_idx];
            tab.metadata_artwork_rx = None;
            tab.metadata_artwork_requested = None;
            match result {
                Ok((node, image)) => {
                    let texture = ctx.load_texture(
                        format!("metadata-artwork-{}-{node}", tab.tab_id),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    tab.metadata_artwork_texture = Some((node, texture));
                }
                Err(error) => {
                    tab.metadata_action_status = Some(format!("Artwork: {error}"));
                }
            }
            ctx.request_repaint();
        }
    }

    fn metadata_live_source_range(
        &self,
        tab_idx: usize,
        document: &MetadataDocument,
        source_path: &Path,
    ) -> Option<(u64, SourceRange)> {
        let mapping = document.audio_mapping.as_ref()?;
        let tab = self.tabs.get(tab_idx)?;
        let document_matches_source =
            same_file::is_same_file(&document.source, source_path).unwrap_or(false);
        if tab.dirty
            || tab.preview_audio_tool.is_some()
            || self.is_virtual_path(&tab.path)
            || !document_matches_source
        {
            return None;
        }
        let current_source = match &self.playback_session.source {
            crate::app::PlaybackSourceKind::EditorTab(path) => path,
            _ => return None,
        };
        if current_source != &tab.path {
            return None;
        }
        let time = self.playback_current_source_time_sec()?;
        let frame = (time.max(0.0) * mapping.sample_rate.max(1) as f64).floor() as u64;
        mapping.frame_range(frame).map(|range| (frame, range))
    }

    fn ui_metadata_structure(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        tab_idx: usize,
        source_path: &Path,
        document: &Arc<MetadataDocument>,
        exact_live: Option<(u64, SourceRange)>,
        source_time: Option<f64>,
    ) {
        draw_live_pcm_status(
            ui,
            document,
            exact_live,
            source_time,
            self.tabs[tab_idx].metadata_hex_page.as_ref(),
        );
        ui.separator();
        let available = ui.available_size();
        let left_width = (available.x * 0.60).max(280.0);
        let selected_before = self.tabs[tab_idx].metadata_selected_node;
        let mut selected = selected_before;
        let play_fraction = exact_live
            .and_then(|(frame, _)| {
                document
                    .audio_mapping
                    .as_ref()
                    .map(|mapping| frame as f32 / mapping.frame_count().max(1) as f32)
            })
            .or_else(|| {
                let duration = self.tabs[tab_idx].samples_len as f64
                    / self.tabs[tab_idx].buffer_sample_rate.max(1) as f64;
                source_time
                    .filter(|_| duration > 0.0)
                    .map(|time| (time / duration).clamp(0.0, 1.0) as f32)
            });
        let waveform = self.tabs[tab_idx].waveform_minmax.clone();
        let hex_page = self.tabs[tab_idx].metadata_hex_page.clone();
        let roots = document.roots.clone();
        let mut pending_action = None;
        let mut open_hex = None;
        let mut requested_page = None;
        let mut add_list_column = None;
        let mut editor_navigation = None;
        egui::Panel::left(egui::Id::new((
            "metadata_structure_panel",
            self.tabs[tab_idx].tab_id,
        )))
        .resizable(true)
        .default_size(left_width)
        .size_range(240.0..=(available.x - 240.0).max(240.0))
        .show_inside(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.heading("Structure");
            egui::ScrollArea::vertical()
                .id_salt(("metadata_tree", self.tabs[tab_idx].tab_id))
                .show(ui, |ui| {
                    for root in roots {
                        draw_metadata_node(
                            ui,
                            document,
                            root,
                            &mut selected,
                            &waveform,
                            play_fraction,
                            exact_live.map(|(_, range)| range),
                            hex_page.as_ref(),
                            0,
                        );
                    }
                });
        });
        if selected != selected_before {
            if let Some(node) = selected.and_then(|id| document.node(id)) {
                self.tabs[tab_idx].metadata_detail_tab = preferred_detail_tab(node);
                self.tabs[tab_idx].metadata_text_encoding = 0;
                self.tabs[tab_idx].metadata_hex_offset = node.payload.file_offset;
            }
        }
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.heading("Details");
            if let Some(node_id) = selected {
                if let Some(node) = document.node(node_id) {
                    requested_page = Some(node.payload.file_offset);
                    let (action, hex_target, column, navigation) = draw_metadata_details(
                        ui,
                        ctx,
                        &mut self.tabs[tab_idx],
                        source_path,
                        document,
                        node,
                        &waveform,
                        play_fraction,
                        hex_page.as_ref(),
                    );
                    pending_action = action;
                    open_hex = hex_target;
                    add_list_column = column;
                    editor_navigation = navigation;
                }
            } else {
                ui.label("Select a structure node.");
            }
        });
        self.tabs[tab_idx].metadata_selected_node = selected;
        if let Some(offset) = requested_page {
            self.request_metadata_hex_page(tab_idx, source_path.to_path_buf(), offset);
        }
        if let Some(offset) = open_hex {
            self.tabs[tab_idx].metadata_hex_offset = offset;
            self.tabs[tab_idx].metadata_sub_view = MetadataSubView::Hex;
        }
        if let Some(action) = pending_action {
            self.start_metadata_action(tab_idx, source_path.to_path_buf(), action);
        }
        if let Some((key, label)) = add_list_column {
            self.add_metadata_list_column(key, label);
        }
        if let Some(navigation) = editor_navigation {
            self.apply_metadata_editor_navigation(tab_idx, document, navigation);
        }
        if let Some(node) = selected.and_then(|node| document.node(node)) {
            if node.content == ContentKind::Image {
                self.request_metadata_artwork(tab_idx, source_path.to_path_buf(), node);
            }
        }
    }

    fn apply_metadata_editor_navigation(
        &mut self,
        tab_idx: usize,
        document: &MetadataDocument,
        navigation: MetadataEditorNavigation,
    ) {
        let source_sample_rate = document
            .audio_mapping
            .as_ref()
            .map(|mapping| mapping.sample_rate)
            .unwrap_or_else(|| self.tabs[tab_idx].buffer_sample_rate)
            .max(1);
        let buffer_sample_rate = self.tabs[tab_idx].buffer_sample_rate.max(1);
        let to_display_sample = |source_sample: u64| {
            ((source_sample as u128)
                .saturating_mul(buffer_sample_rate as u128)
                .checked_div(source_sample_rate as u128)
                .unwrap_or(0))
            .min(usize::MAX as u128) as usize
        };
        let mut seek_time = None;
        {
            let tab = &mut self.tabs[tab_idx];
            match navigation {
                MetadataEditorNavigation::Sample(sample) => {
                    let display_sample = to_display_sample(sample).min(tab.samples_len);
                    tab.preview_offset_samples = Some(display_sample);
                    seek_time = Some(sample as f64 / source_sample_rate as f64);
                }
                MetadataEditorNavigation::Range {
                    start,
                    end_inclusive,
                } => {
                    let start = to_display_sample(start).min(tab.samples_len);
                    let end = to_display_sample(end_inclusive.saturating_add(1))
                        .min(tab.samples_len)
                        .max(start);
                    tab.selection = (end > start).then_some((start, end));
                    tab.preview_offset_samples = Some(start);
                    seek_time = Some(start as f64 / buffer_sample_rate as f64);
                }
                MetadataEditorNavigation::Tempo(bpm) => {
                    tab.bpm_enabled = true;
                    tab.bpm_value = bpm.max(0.0);
                    tab.bpm_user_set = true;
                }
            }
            tab.primary_view = EditorPrimaryView::Wave;
        }
        if let Some(source_time) = seek_time {
            let current_tab_source = matches!(
                &self.playback_session.source,
                crate::app::PlaybackSourceKind::EditorTab(path)
                    if path == &self.tabs[tab_idx].path
            );
            if current_tab_source {
                self.playback_seek_to_source_time(self.mode, source_time);
            }
        }
    }

    fn ui_metadata_hex(
        &mut self,
        ui: &mut egui::Ui,
        _ctx: &egui::Context,
        tab_idx: usize,
        source_path: &Path,
        document: &Arc<MetadataDocument>,
        exact_live: Option<(u64, SourceRange)>,
        source_time: Option<f64>,
    ) {
        let audio_timeline = MetadataAudioTimeline::from_document(document);
        let source_duration = metadata_source_duration_secs(
            document,
            self.tabs[tab_idx].samples_len,
            self.tabs[tab_idx].buffer_sample_rate,
        );
        let playback_fraction = source_time
            .zip(source_duration)
            .filter(|(_, duration)| *duration > 0.0)
            .map(|(time, duration)| (time / duration).clamp(0.0, 1.0));
        let waveform_cursor_fraction =
            playback_fraction.or(self.tabs[tab_idx].metadata_hex_seek_fraction);
        let playback_offset = exact_live.map(|(_, range)| range.offset).or_else(|| {
            playback_fraction.and_then(|fraction| audio_timeline.file_offset_for_fraction(fraction))
        });
        if self.tabs[tab_idx].metadata_follow_playback {
            if let Some(offset) = playback_offset {
                self.tabs[tab_idx].metadata_hex_offset = offset;
            }
        }
        let live_direction = playback_offset.and_then(|offset| {
            let page = self.tabs[tab_idx].metadata_hex_page.as_ref()?;
            if offset < page.start {
                Some("← playhead")
            } else if offset >= page.start.saturating_add(page.bytes.len() as u64) {
                Some("playhead →")
            } else {
                None
            }
        });
        let mut jump_to_live = false;
        let mut show_in_structure = None;
        ui.horizontal_wrapped(|ui| {
            ui.label("Offset");
            let mut offset = self.tabs[tab_idx].metadata_hex_offset;
            if ui
                .add(
                    egui::DragValue::new(&mut offset)
                        .range(0..=document.file_len.saturating_sub(1))
                        .speed(16),
                )
                .changed()
            {
                self.tabs[tab_idx].metadata_hex_offset = offset;
            }
            ui.monospace(format!("0x{:X}", self.tabs[tab_idx].metadata_hex_offset));
            ui.separator();
            ui.selectable_value(
                &mut self.tabs[tab_idx].metadata_hex_bytes_per_row,
                16,
                "16 bytes",
            );
            ui.selectable_value(
                &mut self.tabs[tab_idx].metadata_hex_bytes_per_row,
                32,
                "32 bytes",
            );
            ui.separator();
            ui.checkbox(
                &mut self.tabs[tab_idx].metadata_follow_playback,
                "再生位置に自動スクロール",
            );
            if playback_offset.is_some() && ui.button("再生位置へ移動").clicked() {
                jump_to_live = true;
            }
            if !self.tabs[tab_idx].metadata_follow_playback {
                if let Some(direction) = live_direction {
                    ui.colored_label(Color32::LIGHT_GREEN, direction);
                }
            }
            if let Some(node) = document.smallest_node_at(self.tabs[tab_idx].metadata_hex_offset) {
                ui.label(RichText::new(&node.path).weak());
                if ui.small_button("Show in Structure").clicked() {
                    show_in_structure = Some(node.id);
                }
            }
        });
        if let Some(node_id) = show_in_structure {
            self.tabs[tab_idx].metadata_selected_node =
                visible_metadata_structure_target(document, node_id);
            self.tabs[tab_idx].metadata_sub_view = MetadataSubView::Structure;
        }
        if jump_to_live {
            if let Some(offset) = playback_offset {
                self.tabs[tab_idx].metadata_hex_offset = offset;
            }
        }
        self.request_metadata_hex_page(
            tab_idx,
            source_path.to_path_buf(),
            self.tabs[tab_idx].metadata_hex_offset,
        );
        ui.separator();

        let selected_range = self.tabs[tab_idx]
            .metadata_selected_node
            .and_then(|id| document.node(id))
            .map(|node| SourceRange {
                offset: node.payload.file_offset,
                length: node.payload.length,
            });
        let byte_selection = self.tabs[tab_idx].metadata_hex_selection;
        let bytes_per_row = self.tabs[tab_idx].metadata_hex_bytes_per_row.max(1);
        let waveform = self.tabs[tab_idx].waveform_minmax.clone();
        let Some(page) = self.tabs[tab_idx].metadata_hex_page.as_ref() else {
            ui.spinner();
            ui.label("Loading hex page...");
            return;
        };
        let row_count = page.bytes.len().div_ceil(bytes_per_row);
        let metrics = HexGridMetrics::new(ui, bytes_per_row, true);
        let grid_height = (ui.available_height() - metrics.row_height).max(120.0);
        let live_range = exact_live.map(|(_, range)| range);
        let playback_row = playback_offset.and_then(|offset| {
            let relative = offset.checked_sub(page.start)?;
            let row = usize::try_from(relative / bytes_per_row as u64).ok()?;
            (row < row_count).then_some(row)
        });
        let explicit_scroll_row =
            self.tabs[tab_idx]
                .metadata_hex_scroll_target
                .and_then(|offset| {
                    let relative = offset.checked_sub(page.start)?;
                    let row = usize::try_from(relative / bytes_per_row as u64).ok()?;
                    (row < row_count).then_some(row)
                });
        let scroll_row = explicit_scroll_row.or_else(|| {
            self.tabs[tab_idx]
                .metadata_follow_playback
                .then_some(playback_row)
                .flatten()
        });
        let auto_scroll_offset = scroll_row.map(|row| {
            centered_hex_row_scroll_offset(
                row,
                row_count,
                metrics.row_height + ui.spacing().item_spacing.y,
                grid_height,
            )
        });
        let mut clicked_range = None;
        let mut row_seek_fraction = None;
        let (header_rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), metrics.row_height),
            egui::Sense::hover(),
        );
        let mut rows_scroll = egui::ScrollArea::both()
            .id_salt((
                "metadata_hex",
                self.tabs[tab_idx].tab_id,
                page.start,
                bytes_per_row,
            ))
            .max_height(grid_height)
            .auto_shrink([false, false]);
        if let Some(offset) = auto_scroll_offset {
            rows_scroll = rows_scroll.vertical_scroll_offset(offset);
        }
        let scroll_output = rows_scroll.show_rows(ui, metrics.row_height, row_count, |ui, rows| {
            ui.set_min_width(metrics.total_width);
            for row in rows {
                let start = row * bytes_per_row;
                let end = (start + bytes_per_row).min(page.bytes.len());
                let absolute = page.start + start as u64;
                let data = &page.bytes[start..end];
                let row_range = SourceRange {
                    offset: absolute,
                    length: data.len() as u64,
                };
                let response = draw_hex_grid_row(
                    ui,
                    &metrics,
                    row,
                    absolute,
                    data,
                    live_range,
                    selected_range,
                    byte_selection,
                );
                if response.double_clicked() {
                    if let Some(fraction) = audio_timeline.fraction_for_range(row_range) {
                        row_seek_fraction = Some(fraction);
                    } else {
                        clicked_range = Some(row_range);
                    }
                } else if response.clicked() {
                    clicked_range = Some(row_range);
                }
            }
        });
        draw_hex_grid_header_at(
            ui,
            &metrics,
            header_rect,
            scroll_output.state.offset.x,
            false,
        );
        let preferred_waveform_left =
            scroll_output.inner_rect.left() + metrics.waveform_x - scroll_output.state.offset.x;
        let waveform_left = preferred_waveform_left
            .min(scroll_output.inner_rect.right() - metrics.waveform_width)
            .max(scroll_output.inner_rect.left());
        let waveform_rect = egui::Rect::from_min_max(
            egui::pos2(waveform_left, scroll_output.inner_rect.top()),
            egui::pos2(
                waveform_left + metrics.waveform_width,
                scroll_output.inner_rect.bottom(),
            ),
        )
        .intersect(scroll_output.inner_rect);
        draw_fixed_hex_waveform_header(ui, header_rect, waveform_rect.x_range());
        let waveform_seek_fraction = draw_hex_vertical_waveform(
            ui,
            waveform_rect,
            &waveform,
            waveform_cursor_fraction,
            page.start,
        );
        if explicit_scroll_row.is_some() {
            self.tabs[tab_idx].metadata_hex_scroll_target = None;
        }
        let requested_seek_fraction = waveform_seek_fraction.or(row_seek_fraction);
        if let Some(fraction) = requested_seek_fraction {
            clicked_range = None;
            let fraction = fraction.clamp(0.0, 1.0);
            self.tabs[tab_idx].metadata_hex_seek_fraction = Some(fraction);
            if let Some(range) =
                metadata_audio_range_for_fraction(document, &audio_timeline, fraction)
            {
                self.tabs[tab_idx].metadata_hex_selection = Some(range);
                self.tabs[tab_idx].metadata_hex_offset = range.offset;
                self.tabs[tab_idx].metadata_hex_scroll_target = Some(range.offset);
                self.tabs[tab_idx].metadata_selected_node =
                    document.smallest_node_at(range.offset).map(|node| node.id);
            }
            let current_tab_source = matches!(
                &self.playback_session.source,
                crate::app::PlaybackSourceKind::EditorTab(path)
                    if path == &self.tabs[tab_idx].path
            );
            if current_tab_source {
                if let Some(duration) = source_duration {
                    self.playback_seek_to_source_time(self.mode, fraction * duration);
                }
            }
            self.request_metadata_hex_page(
                tab_idx,
                source_path.to_path_buf(),
                self.tabs[tab_idx].metadata_hex_offset,
            );
        }
        if let Some(range) = clicked_range {
            self.tabs[tab_idx].metadata_hex_selection = Some(range);
            self.tabs[tab_idx].metadata_hex_offset = range.offset;
            self.tabs[tab_idx].metadata_selected_node =
                document.smallest_node_at(range.offset).map(|node| node.id);
        }
    }

    fn request_metadata_hex_page(&mut self, tab_idx: usize, path: PathBuf, offset: u64) {
        let start = offset / HEX_PAGE_BYTES as u64 * HEX_PAGE_BYTES as u64;
        let tab = &mut self.tabs[tab_idx];
        if let Some(index) = tab
            .metadata_hex_pages
            .iter()
            .position(|page| page.start == start)
        {
            if let Some(page) = tab.metadata_hex_pages.remove(index) {
                tab.metadata_hex_page = Some(page.clone());
                tab.metadata_hex_pages.push_back(page);
            }
            return;
        }
        if tab
            .metadata_hex_page
            .as_ref()
            .is_some_and(|page| page.start == start)
            || tab.metadata_hex_page_requested == Some(start)
        {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        tab.metadata_hex_page_requested = Some(start);
        tab.metadata_hex_page_rx = Some(rx);
        std::thread::spawn(move || {
            let result = crate::metadata::read_payload_page(&path, start, HEX_PAGE_BYTES, None)
                .map(|bytes| {
                    let mut pages = vec![(start, bytes)];
                    let adjacent = [
                        start.checked_sub(HEX_PAGE_BYTES as u64),
                        start.checked_add(HEX_PAGE_BYTES as u64),
                    ];
                    for page_start in adjacent.into_iter().flatten() {
                        if let Ok(bytes) = crate::metadata::read_payload_page(
                            &path,
                            page_start,
                            HEX_PAGE_BYTES,
                            None,
                        ) {
                            pages.push((page_start, bytes));
                        }
                    }
                    pages
                })
                .map_err(|error| error.to_string());
            let _ = tx.send(result);
        });
    }

    fn start_metadata_action(
        &mut self,
        tab_idx: usize,
        path: PathBuf,
        action: PendingMetadataAction,
    ) {
        if self.tabs[tab_idx].metadata_action_rx.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let total = match &action {
            PendingMetadataAction::Search { payload, .. }
            | PendingMetadataAction::Hash { payload }
            | PendingMetadataAction::Copy { payload }
            | PendingMetadataAction::Extract { payload, .. } => payload.length,
        };
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let progress = Arc::new(std::sync::atomic::AtomicU64::new(0));
        self.tabs[tab_idx].metadata_action_rx = Some(rx);
        self.tabs[tab_idx].metadata_action_cancel = Some(Arc::clone(&cancel));
        self.tabs[tab_idx].metadata_action_progress = Some(Arc::clone(&progress));
        self.tabs[tab_idx].metadata_action_total = total;
        self.tabs[tab_idx].metadata_action_status = Some("Working...".to_string());
        std::thread::spawn(move || {
            let result = match action {
                PendingMetadataAction::Search {
                    payload,
                    kind,
                    query,
                } => crate::metadata::parse_search_pattern(kind, &query)
                    .and_then(|pattern| {
                        crate::metadata::search_payload_with_progress(
                            &path,
                            payload,
                            &pattern,
                            crate::metadata::PAYLOAD_SEARCH_RESULT_LIMIT,
                            Some(&cancel),
                            Some(&progress),
                        )
                    })
                    .map(MetadataActionResult::Search),
                PendingMetadataAction::Hash { payload } => {
                    crate::metadata::hash_payload_with_progress(
                        &path,
                        payload,
                        HashAlgorithm::Sha256,
                        Some(&cancel),
                        Some(&progress),
                    )
                    .map(MetadataActionResult::Hash)
                }
                PendingMetadataAction::Copy { payload } => crate::metadata::read_payload(
                    &path,
                    payload,
                    crate::metadata::PAYLOAD_COPY_LIMIT,
                )
                .map(|bytes| {
                    progress.store(payload.length, std::sync::atomic::Ordering::Relaxed);
                    MetadataActionResult::CopyHex(
                        bytes
                            .iter()
                            .map(|byte| format!("{byte:02X}"))
                            .collect::<Vec<_>>()
                            .join(" "),
                    )
                }),
                PendingMetadataAction::Extract {
                    payload,
                    output,
                    overwrite,
                } => crate::metadata::extract_payload_with_progress(
                    &path,
                    payload,
                    &output,
                    overwrite,
                    Some(&cancel),
                    Some(&progress),
                )
                .map(|_| MetadataActionResult::Extracted(output)),
            }
            .map_err(|error| error.to_string());
            let _ = tx.send(result);
        });
    }

    fn request_metadata_artwork(&mut self, tab_idx: usize, path: PathBuf, node: &MetadataNode) {
        let tab = &mut self.tabs[tab_idx];
        if tab
            .metadata_artwork_texture
            .as_ref()
            .is_some_and(|(node_id, _)| *node_id == node.id)
            || tab.metadata_artwork_requested == Some(node.id)
        {
            return;
        }
        let node_id = node.id;
        let payload = node.payload;
        let (tx, rx) = std::sync::mpsc::channel();
        tab.metadata_artwork_requested = Some(node_id);
        tab.metadata_artwork_rx = Some(rx);
        std::thread::spawn(move || {
            let result =
                crate::metadata::read_payload(&path, payload, crate::metadata::PAYLOAD_COPY_LIMIT)
                    .and_then(|bytes| {
                        let image = image::load_from_memory(&bytes)?;
                        let rgba = image.thumbnail(1024, 1024).to_rgba8();
                        let size = [rgba.width() as usize, rgba.height() as usize];
                        Ok((
                            node_id,
                            egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()),
                        ))
                    })
                    .map_err(|error| error.to_string());
            let _ = tx.send(result);
        });
    }
}

fn draw_metadata_node(
    ui: &mut egui::Ui,
    document: &MetadataDocument,
    node_id: NodeId,
    selected: &mut Option<NodeId>,
    waveform: &[(f32, f32)],
    play_fraction: Option<f32>,
    live_range: Option<SourceRange>,
    hex_page: Option<&MetadataHexPage>,
    depth: usize,
) {
    if depth > 128 {
        ui.colored_label(Color32::YELLOW, "Depth limit");
        return;
    }
    let Some(node) = document.node(node_id) else {
        return;
    };
    if metadata_node_hidden_in_structure(document.container, node) {
        return;
    }
    let live = live_range.is_some_and(|range| {
        ranges_overlap(
            range,
            SourceRange {
                offset: node.payload.file_offset,
                length: node.payload.length,
            },
        )
    });
    let selected_now = *selected == Some(node_id);
    let accent = if live {
        Color32::LIGHT_GREEN
    } else if !node.known {
        Color32::from_rgb(235, 185, 72)
    } else if selected_now {
        Color32::from_rgb(98, 171, 235)
    } else {
        metadata_accent()
    };
    let preview_label = metadata_preview_label(node);
    let preview_value = metadata_preview_value(node);
    let id = ui.make_persistent_id(("metadata_node", node_id));
    let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(),
        id,
        node.parent.is_none(),
    );
    let header_response = ui.horizontal(|ui| {
        state.show_toggle_button(ui, egui::collapsing_header::paint_default_icon);
        let header = ui.vertical(|ui| {
            let name_response = ui
                .horizontal_wrapped(|ui| {
                    let response = ui.add(
                        egui::Label::new(RichText::new(&node.name).strong().color(
                            if selected_now {
                                Color32::WHITE
                            } else {
                                metadata_value_text()
                            },
                        ))
                        .sense(egui::Sense::click()),
                    );
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            ui.is_enabled(),
                            node.name.trim().to_owned(),
                        )
                    });
                    if live {
                        ui.label(
                            RichText::new("● LIVE")
                                .small()
                                .strong()
                                .color(Color32::LIGHT_GREEN),
                        );
                    }
                    if !node.known {
                        ui.label(
                            RichText::new("UNKNOWN")
                                .small()
                                .strong()
                                .color(Color32::from_rgb(235, 185, 72)),
                        );
                    }
                    response
                })
                .inner;
            draw_metadata_value_card(ui, preview_label, &preview_value, accent, false);
            name_response
        });
        let card_response = ui.interact(
            header.response.rect,
            id.with("select"),
            egui::Sense::click(),
        );
        header.inner.union(card_response)
    });
    let header_clicked = header_response.inner.clicked();
    if header_clicked {
        state.toggle(ui);
    }
    state.show_body_indented(&header_response.response, ui, |ui| {
        match node.content {
            ContentKind::Audio => {
                draw_metadata_surface(ui, "AUDIO PAYLOAD", metadata_accent(), |ui| {
                    draw_inline_waveform(ui, waveform, play_fraction);
                });
            }
            ContentKind::Text => {
                if let Some(summary) = &node.summary {
                    let preview: String = summary
                        .lines()
                        .take(3)
                        .collect::<Vec<_>>()
                        .join("\n")
                        .chars()
                        .take(512)
                        .collect();
                    draw_metadata_surface(ui, "TEXT PREVIEW", metadata_accent(), |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(preview).color(metadata_value_text()))
                                .wrap(),
                        );
                    });
                }
            }
            ContentKind::Binary | ContentKind::Image => {
                if let Some(bytes) = hex_page.and_then(|page| {
                    bytes_for_range(
                        page,
                        SourceRange {
                            offset: node.payload.file_offset,
                            length: node.payload.length.min(32),
                        },
                    )
                }) {
                    draw_binary_preview(ui, bytes, node.payload.file_offset, live_range);
                } else {
                    draw_metadata_value_card(
                        ui,
                        "BINARY PAYLOAD",
                        &format!(
                            "{} bytes · starts at 0x{:X}",
                            node.payload.length, node.payload.file_offset
                        ),
                        Color32::from_rgb(116, 159, 211),
                        false,
                    );
                }
            }
            ContentKind::Xml if node.children.is_empty() => {
                ui.label(RichText::new("XML payload (deferred or empty)").weak());
            }
            _ => {}
        }
        for &child in &node.children {
            draw_metadata_node(
                ui,
                document,
                child,
                selected,
                waveform,
                play_fraction,
                live_range,
                hex_page,
                depth + 1,
            );
        }
    });
    if header_clicked {
        *selected = Some(node_id);
    }
}

fn metadata_preview_label(node: &MetadataNode) -> &'static str {
    match node.content {
        ContentKind::Container => "CONTAINER",
        ContentKind::Audio => "AUDIO DATA",
        ContentKind::Xml => "XML",
        ContentKind::Text => "TEXT",
        ContentKind::Binary => "BINARY",
        ContentKind::Image => "ARTWORK",
        ContentKind::Padding => "PADDING",
    }
}

fn metadata_preview_value(node: &MetadataNode) -> String {
    let value = node.summary.as_deref().unwrap_or_default().trim();
    if value.is_empty() {
        return format!(
            "{} bytes · offset 0x{:X}",
            node.payload.length, node.payload.file_offset
        );
    }
    let flattened = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut preview = flattened.chars().take(160).collect::<String>();
    if flattened.chars().count() > 160 {
        preview.push('…');
    }
    preview
}

fn draw_metadata_value_card(
    ui: &mut egui::Ui,
    label: &str,
    value: &str,
    accent: Color32,
    monospace: bool,
) {
    draw_metadata_surface(ui, label, accent, |ui| {
        let text = RichText::new(value).color(metadata_value_text()).size(13.5);
        ui.add(
            egui::Label::new(if monospace { text.monospace() } else { text })
                .wrap()
                .selectable(true),
        );
    });
}

fn draw_metadata_surface<R>(
    ui: &mut egui::Ui,
    label: &str,
    accent: Color32,
    add_content: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let response = egui::Frame::NONE
        .fill(metadata_surface_fill())
        .stroke(Stroke::new(1.0, metadata_surface_stroke()))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new(label).small().strong().color(accent));
            ui.add_space(3.0);
            add_content(ui)
        });
    let bar = egui::Rect::from_min_max(
        response.response.rect.left_top() + egui::vec2(1.0, 6.0),
        egui::pos2(
            response.response.rect.left() + 4.0,
            response.response.rect.bottom() - 6.0,
        ),
    );
    ui.painter().rect_filled(bar, 2.0, accent);
    response
}

#[derive(Clone)]
struct HexGridMetrics {
    font_id: egui::FontId,
    char_width: f32,
    row_height: f32,
    bytes_per_row: usize,
    address_x: f32,
    hex_x: f32,
    ascii_x: f32,
    byte_stride: f32,
    ascii_stride: f32,
    group_gap: f32,
    waveform_x: f32,
    waveform_width: f32,
    total_width: f32,
}

impl HexGridMetrics {
    fn new(ui: &egui::Ui, bytes_per_row: usize, show_waveform: bool) -> Self {
        let font_id = egui::TextStyle::Monospace.resolve(ui.style());
        let char_width = ui
            .painter()
            .fonts_mut(|fonts| fonts.glyph_width(&font_id, '0'))
            .max(7.0);
        let row_height = ui
            .text_style_height(&egui::TextStyle::Monospace)
            .max(font_id.size)
            + 8.0;
        let bytes_per_row = bytes_per_row.max(1);
        let address_x = 8.0;
        let byte_stride = char_width * 3.15;
        let ascii_stride = char_width * 1.35;
        let group_gap = char_width * 1.2;
        let hex_x = address_x + char_width * 16.0 + char_width * 3.0;
        let group_count = bytes_per_row.saturating_sub(1) / 8;
        let hex_width = bytes_per_row as f32 * byte_stride + group_count as f32 * group_gap;
        let ascii_x = hex_x + hex_width + char_width * 2.5;
        let ascii_right = ascii_x + bytes_per_row as f32 * ascii_stride;
        let waveform_x = ascii_right + char_width * 3.0;
        let waveform_width = if show_waveform {
            HEX_WAVEFORM_RAIL_WIDTH
        } else {
            0.0
        };
        let total_width = if show_waveform {
            waveform_x + waveform_width + 12.0
        } else {
            ascii_right + 10.0
        };
        Self {
            font_id,
            char_width,
            row_height,
            bytes_per_row,
            address_x,
            hex_x,
            ascii_x,
            byte_stride,
            ascii_stride,
            group_gap,
            waveform_x,
            waveform_width,
            total_width,
        }
    }

    fn byte_x(&self, index: usize) -> f32 {
        self.hex_x + index as f32 * self.byte_stride + (index / 8) as f32 * self.group_gap
    }

    fn ascii_byte_x(&self, index: usize) -> f32 {
        self.ascii_x + index as f32 * self.ascii_stride
    }
}

fn draw_hex_grid_header(ui: &mut egui::Ui, metrics: &HexGridMetrics, show_source_time: bool) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(metrics.total_width, metrics.row_height),
        egui::Sense::hover(),
    );
    draw_hex_grid_header_at(ui, metrics, rect, 0.0, show_source_time);
}

fn draw_hex_grid_header_at(
    ui: &mut egui::Ui,
    metrics: &HexGridMetrics,
    rect: egui::Rect,
    horizontal_offset: f32,
    show_source_time: bool,
) {
    let painter = ui.painter().with_clip_rect(rect);
    let content_left = rect.left() - horizontal_offset;
    painter.rect_filled(rect, 4.0, Color32::from_rgb(23, 29, 38));
    painter.text(
        egui::pos2(content_left + metrics.address_x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "OFFSET",
        metrics.font_id.clone(),
        Color32::from_rgb(121, 139, 158),
    );
    for index in 0..metrics.bytes_per_row {
        painter.text(
            egui::pos2(
                content_left + metrics.byte_x(index) + metrics.char_width,
                rect.center().y,
            ),
            egui::Align2::CENTER_CENTER,
            format!("{index:02X}"),
            metrics.font_id.clone(),
            Color32::from_rgb(121, 139, 158),
        );
    }
    painter.text(
        egui::pos2(content_left + metrics.ascii_x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "ASCII",
        metrics.font_id.clone(),
        Color32::from_rgb(121, 139, 158),
    );
    if metrics.waveform_width > 0.0 && show_source_time {
        painter.line_segment(
            [
                egui::pos2(
                    content_left + metrics.waveform_x - metrics.char_width * 1.5,
                    rect.top() + 2.0,
                ),
                egui::pos2(
                    content_left + metrics.waveform_x - metrics.char_width * 1.5,
                    rect.bottom() - 2.0,
                ),
            ],
            Stroke::new(1.0, metadata_surface_stroke()),
        );
        painter.text(
            egui::pos2(content_left + metrics.waveform_x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            if show_source_time {
                "WAVE / SEEK · SOURCE TIME"
            } else {
                "WAVE"
            },
            egui::TextStyle::Small.resolve(ui.style()),
            Color32::from_rgb(91, 201, 174),
        );
    }
    painter.line_segment(
        [
            egui::pos2(rect.left(), rect.bottom()),
            egui::pos2(rect.right(), rect.bottom()),
        ],
        Stroke::new(1.0, metadata_surface_stroke()),
    );
}

fn draw_fixed_hex_waveform_header(
    ui: &egui::Ui,
    header_rect: egui::Rect,
    waveform_x: egui::Rangef,
) {
    if waveform_x.max <= waveform_x.min {
        return;
    }
    let rect = egui::Rect::from_x_y_ranges(waveform_x, header_rect.y_range());
    let painter = ui.painter().with_clip_rect(header_rect);
    painter.rect_filled(rect, 0.0, Color32::from_rgb(18, 31, 34));
    painter.line_segment(
        [rect.left_top(), rect.left_bottom()],
        Stroke::new(1.0, metadata_surface_stroke()),
    );
    painter.text(
        egui::pos2(rect.left() + 10.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "WAVE / SEEK · SOURCE TIME",
        egui::TextStyle::Small.resolve(ui.style()),
        Color32::from_rgb(91, 201, 174),
    );
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, metadata_surface_stroke()),
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_hex_grid_row(
    ui: &mut egui::Ui,
    metrics: &HexGridMetrics,
    row_index: usize,
    absolute: u64,
    data: &[u8],
    live_range: Option<SourceRange>,
    selected_range: Option<SourceRange>,
    byte_selection: Option<SourceRange>,
) -> egui::Response {
    let row_range = SourceRange {
        offset: absolute,
        length: data.len() as u64,
    };
    let live = live_range.is_some_and(|range| ranges_overlap(row_range, range));
    let selected = selected_range.is_some_and(|range| ranges_overlap(row_range, range));
    let byte_selected = byte_selection.is_some_and(|range| ranges_overlap(row_range, range));
    let base_fill = if live {
        Color32::from_rgb(22, 67, 55)
    } else if byte_selected {
        Color32::from_rgb(63, 50, 31)
    } else if selected {
        Color32::from_rgb(31, 42, 58)
    } else if row_index % 2 == 0 {
        Color32::from_rgb(15, 19, 25)
    } else {
        Color32::from_rgb(18, 23, 30)
    };
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(metrics.total_width, metrics.row_height),
        egui::Sense::click(),
    );
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, base_fill);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            format!("Hex row {absolute:016X}"),
        )
    });
    painter.text(
        egui::pos2(rect.left() + metrics.address_x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        format!("{absolute:016X}"),
        metrics.font_id.clone(),
        if live {
            Color32::from_rgb(155, 238, 193)
        } else {
            Color32::from_rgb(165, 179, 194)
        },
    );

    for (index, byte) in data.iter().enumerate() {
        let byte_range = SourceRange {
            offset: absolute.saturating_add(index as u64),
            length: 1,
        };
        let byte_live = live_range.is_some_and(|range| ranges_overlap(byte_range, range));
        let byte_marked = byte_selection.is_some_and(|range| ranges_overlap(byte_range, range));
        let byte_fill = if byte_live {
            Some(Color32::from_rgb(39, 126, 95))
        } else if byte_marked {
            Some(Color32::from_rgb(128, 87, 32))
        } else {
            None
        };
        let hex_cell = egui::Rect::from_center_size(
            egui::pos2(
                rect.left() + metrics.byte_x(index) + metrics.char_width,
                rect.center().y,
            ),
            egui::vec2(metrics.char_width * 2.55, metrics.row_height - 4.0),
        );
        if let Some(fill) = byte_fill {
            painter.rect_filled(hex_cell, 3.0, fill);
        }
        painter.text(
            hex_cell.center(),
            egui::Align2::CENTER_CENTER,
            format!("{byte:02X}"),
            metrics.font_id.clone(),
            Color32::from_rgb(232, 237, 243),
        );

        let ascii_cell = egui::Rect::from_center_size(
            egui::pos2(
                rect.left() + metrics.ascii_byte_x(index) + metrics.ascii_stride * 0.5,
                rect.center().y,
            ),
            egui::vec2(metrics.ascii_stride, metrics.row_height - 4.0),
        );
        if let Some(fill) = byte_fill {
            painter.rect_filled(ascii_cell, 2.0, fill);
        }
        painter.text(
            ascii_cell.center(),
            egui::Align2::CENTER_CENTER,
            printable_ascii(*byte),
            metrics.font_id.clone(),
            Color32::from_rgb(218, 225, 233),
        );
    }

    let separator_x = rect.left() + metrics.ascii_x - metrics.char_width * 1.25;
    painter.line_segment(
        [
            egui::pos2(separator_x, rect.top() + 2.0),
            egui::pos2(separator_x, rect.bottom() - 2.0),
        ],
        Stroke::new(1.0, metadata_surface_stroke()),
    );
    if metrics.waveform_width > 0.0 {
        let waveform_separator_x = rect.left() + metrics.waveform_x - metrics.char_width * 1.5;
        painter.line_segment(
            [
                egui::pos2(waveform_separator_x, rect.top() + 2.0),
                egui::pos2(waveform_separator_x, rect.bottom() - 2.0),
            ],
            Stroke::new(1.0, metadata_surface_stroke()),
        );
    }
    if live {
        let live_bar = egui::Rect::from_min_max(
            rect.left_top(),
            egui::pos2(rect.left() + 3.0, rect.bottom()),
        );
        painter.rect_filled(live_bar, 0.0, Color32::LIGHT_GREEN);
    }
    response.on_hover_text("音声Byte行をダブルクリックして波形位置へシーク")
}

fn draw_binary_preview(
    ui: &mut egui::Ui,
    bytes: &[u8],
    absolute: u64,
    live_range: Option<SourceRange>,
) {
    draw_metadata_surface(
        ui,
        "BINARY · 32 BYTE PREVIEW",
        Color32::from_rgb(116, 159, 211),
        |ui| {
            let metrics = HexGridMetrics::new(ui, 16, false);
            egui::ScrollArea::horizontal()
                .id_salt(("metadata_binary_preview", absolute))
                .show(ui, |ui| {
                    ui.set_min_width(metrics.total_width);
                    draw_hex_grid_header(ui, &metrics, false);
                    for (row, data) in bytes.chunks(16).enumerate() {
                        draw_hex_grid_row(
                            ui,
                            &metrics,
                            row,
                            absolute.saturating_add((row * 16) as u64),
                            data,
                            live_range,
                            None,
                            None,
                        );
                    }
                });
        },
    );
}

fn printable_ascii(byte: u8) -> char {
    if byte.is_ascii_graphic() || byte == b' ' {
        byte as char
    } else {
        '.'
    }
}

#[derive(Clone, Debug)]
struct MetadataAudioTimeline {
    ranges: Vec<SourceRange>,
    total_bytes: u64,
}

impl MetadataAudioTimeline {
    fn from_document(document: &MetadataDocument) -> Self {
        if let Some(mapping) = document
            .audio_mapping
            .as_ref()
            .filter(|mapping| mapping.data_length > 0)
        {
            return Self {
                ranges: vec![SourceRange {
                    offset: mapping.data_offset,
                    length: mapping.data_length,
                }],
                total_bytes: mapping.data_length,
            };
        }

        let mut ranges: Vec<SourceRange> = document
            .nodes
            .iter()
            .filter(|node| node.content == ContentKind::Audio && node.payload.length > 0)
            .map(|node| SourceRange {
                offset: node.payload.file_offset,
                length: node.payload.length,
            })
            .collect();
        ranges.sort_by_key(|range| range.offset);
        let mut merged: Vec<SourceRange> = Vec::with_capacity(ranges.len());
        for range in ranges {
            if let Some(last) = merged.last_mut() {
                let last_end = last.offset.saturating_add(last.length);
                if range.offset <= last_end {
                    let merged_end = last_end.max(range.offset.saturating_add(range.length));
                    last.length = merged_end.saturating_sub(last.offset);
                    continue;
                }
            }
            merged.push(range);
        }
        let total_bytes = merged
            .iter()
            .fold(0u64, |total, range| total.saturating_add(range.length));
        Self {
            ranges: merged,
            total_bytes,
        }
    }

    fn fraction_span(&self, file_range: SourceRange) -> Option<(f64, f64)> {
        if self.total_bytes == 0 || file_range.length == 0 {
            return None;
        }
        let file_end = file_range.offset.saturating_add(file_range.length);
        let mut cumulative = 0u64;
        let mut first = None;
        let mut last = None;
        for audio in &self.ranges {
            let audio_end = audio.offset.saturating_add(audio.length);
            let start = file_range.offset.max(audio.offset);
            let end = file_end.min(audio_end);
            if start < end {
                let timeline_start = cumulative.saturating_add(start.saturating_sub(audio.offset));
                let timeline_end = cumulative.saturating_add(end.saturating_sub(audio.offset));
                first.get_or_insert(timeline_start);
                last = Some(timeline_end);
            }
            cumulative = cumulative.saturating_add(audio.length);
        }
        Some((
            first? as f64 / self.total_bytes as f64,
            last? as f64 / self.total_bytes as f64,
        ))
    }

    fn fraction_for_range(&self, file_range: SourceRange) -> Option<f64> {
        let (start, end) = self.fraction_span(file_range)?;
        Some(((start + end) * 0.5).clamp(0.0, 1.0))
    }

    fn file_offset_for_fraction(&self, fraction: f64) -> Option<u64> {
        if self.total_bytes == 0 {
            return None;
        }
        let target = ((fraction.clamp(0.0, 1.0) * self.total_bytes as f64).floor() as u64)
            .min(self.total_bytes.saturating_sub(1));
        let mut cumulative = 0u64;
        for range in &self.ranges {
            let next = cumulative.saturating_add(range.length);
            if target < next {
                return range.offset.checked_add(target.saturating_sub(cumulative));
            }
            cumulative = next;
        }
        None
    }
}

fn metadata_audio_range_for_fraction(
    document: &MetadataDocument,
    timeline: &MetadataAudioTimeline,
    fraction: f64,
) -> Option<SourceRange> {
    if let Some(mapping) = document.audio_mapping.as_ref() {
        let frame_count = mapping.frame_count();
        if frame_count == 0 {
            return None;
        }
        let frame = ((fraction.clamp(0.0, 1.0) * frame_count as f64).floor() as u64)
            .min(frame_count.saturating_sub(1));
        return mapping.frame_range(frame);
    }
    timeline
        .file_offset_for_fraction(fraction)
        .map(|offset| SourceRange { offset, length: 1 })
}

fn metadata_source_duration_secs(
    document: &MetadataDocument,
    decoded_frames: usize,
    decoded_sample_rate: u32,
) -> Option<f64> {
    if let Some(mapping) = document.audio_mapping.as_ref() {
        return (mapping.sample_rate > 0)
            .then(|| mapping.frame_count() as f64 / mapping.sample_rate.max(1) as f64);
    }
    (decoded_frames > 0 && decoded_sample_rate > 0)
        .then(|| decoded_frames as f64 / decoded_sample_rate as f64)
}

fn waveform_peak_for_fraction_span(
    waveform: &[(f32, f32)],
    start: f64,
    end: f64,
) -> Option<(f32, f32)> {
    if waveform.is_empty() {
        return None;
    }
    let start_index = ((start.clamp(0.0, 1.0) * waveform.len() as f64).floor() as usize)
        .min(waveform.len().saturating_sub(1));
    let end_index = ((end.clamp(0.0, 1.0) * waveform.len() as f64).ceil() as usize)
        .clamp(start_index + 1, waveform.len());
    let mut min = 1.0f32;
    let mut max = -1.0f32;
    for &(sample_min, sample_max) in &waveform[start_index..end_index] {
        min = min.min(sample_min);
        max = max.max(sample_max);
    }
    Some((min.clamp(-1.0, 1.0), max.clamp(-1.0, 1.0)))
}

fn draw_hex_vertical_waveform(
    ui: &mut egui::Ui,
    rail_rect: egui::Rect,
    waveform: &[(f32, f32)],
    playback_fraction: Option<f64>,
    page_start: u64,
) -> Option<f64> {
    if rail_rect.width() <= 1.0 || rail_rect.height() <= 1.0 {
        return None;
    }
    let painter = ui.painter().with_clip_rect(rail_rect);
    painter.rect_filled(rail_rect, 0.0, Color32::from_rgb(11, 20, 23));
    painter.line_segment(
        [rail_rect.left_top(), rail_rect.left_bottom()],
        Stroke::new(1.0, metadata_surface_stroke()),
    );
    painter.line_segment(
        [
            egui::pos2(rail_rect.center().x, rail_rect.top()),
            egui::pos2(rail_rect.center().x, rail_rect.bottom()),
        ],
        Stroke::new(1.0, Color32::from_rgb(35, 71, 68)),
    );

    let half_width = rail_rect.width() * 0.46;
    let mut previous: Option<(egui::Pos2, egui::Pos2)> = None;
    let points = (rail_rect.height().max(1.0) as usize).min(waveform.len());
    for index in 0..points {
        let start = index as f64 / points.max(1) as f64;
        let end = (index + 1) as f64 / points.max(1) as f64;
        let Some((min, max)) = waveform_peak_for_fraction_span(waveform, start, end) else {
            continue;
        };
        let y = rail_rect.top() + (index as f32 + 0.5) / points.max(1) as f32 * rail_rect.height();
        let left = egui::pos2(rail_rect.center().x + min * half_width, y);
        let right = egui::pos2(rail_rect.center().x + max * half_width, y);
        if let Some((previous_left, previous_right)) = previous {
            painter.add(egui::Shape::convex_polygon(
                vec![previous_left, previous_right, right, left],
                Color32::from_rgba_unmultiplied(49, 178, 151, 42),
                Stroke::NONE,
            ));
            painter.line_segment(
                [previous_left, left],
                Stroke::new(1.2, Color32::from_rgb(69, 209, 176)),
            );
            painter.line_segment(
                [previous_right, right],
                Stroke::new(1.2, Color32::from_rgb(69, 209, 176)),
            );
        } else {
            painter.line_segment(
                [left, right],
                Stroke::new(1.2, Color32::from_rgb(69, 209, 176)),
            );
        }
        previous = Some((left, right));
    }

    if waveform.is_empty() {
        painter.text(
            rail_rect.center(),
            egui::Align2::CENTER_CENTER,
            "Waveform loading...",
            egui::TextStyle::Small.resolve(ui.style()),
            Color32::GRAY,
        );
    }
    if let Some(playback) = playback_fraction {
        let y = rail_rect.top() + playback.clamp(0.0, 1.0) as f32 * rail_rect.height();
        painter.line_segment(
            [
                egui::pos2(rail_rect.left(), y),
                egui::pos2(rail_rect.right(), y),
            ],
            Stroke::new(1.8, Color32::WHITE),
        );
        painter.circle_filled(egui::pos2(rail_rect.right() - 6.0, y), 3.5, Color32::WHITE);
    }

    let response = ui
        .interact(
            rail_rect,
            ui.make_persistent_id(("metadata_hex_waveform_seek", page_start)),
            egui::Sense::click_and_drag(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("クリックまたはドラッグしてsource timeへシーク");
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Slider,
            ui.is_enabled(),
            "縦波形シークバー",
        )
    });
    if !(response.clicked() || response.dragged()) {
        return None;
    }
    let pointer = response.interact_pointer_pos()?;
    Some(((pointer.y - rail_rect.top()) / rail_rect.height()).clamp(0.0, 1.0) as f64)
}

fn draw_inline_waveform(ui: &mut egui::Ui, waveform: &[(f32, f32)], play_fraction: Option<f32>) {
    let desired = egui::vec2(ui.available_width().min(520.0), 72.0);
    let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, 3.0, Color32::from_rgb(18, 23, 29));
    if waveform.is_empty() {
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Waveform loading...",
            egui::TextStyle::Small.resolve(ui.style()),
            Color32::GRAY,
        );
        return;
    }
    let points = waveform.len().min(rect.width().max(1.0) as usize);
    for x in 0..points {
        let idx = x * waveform.len() / points;
        let (min, max) = waveform[idx];
        let px = rect.left() + x as f32 / points.max(1) as f32 * rect.width();
        let y1 = rect.center().y - max.clamp(-1.0, 1.0) * rect.height() * 0.45;
        let y2 = rect.center().y - min.clamp(-1.0, 1.0) * rect.height() * 0.45;
        ui.painter().line_segment(
            [egui::pos2(px, y1), egui::pos2(px, y2)],
            Stroke::new(1.0, Color32::from_rgb(80, 195, 165)),
        );
    }
    if let Some(fraction) = play_fraction {
        let x = rect.left() + fraction.clamp(0.0, 1.0) * rect.width();
        ui.painter().line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            Stroke::new(1.5, Color32::WHITE),
        );
    }
}

fn draw_metadata_details(
    ui: &mut egui::Ui,
    _ctx: &egui::Context,
    tab: &mut crate::app::types::EditorTab,
    source_path: &Path,
    document: &MetadataDocument,
    node: &MetadataNode,
    waveform: &[(f32, f32)],
    play_fraction: Option<f32>,
    hex_page: Option<&MetadataHexPage>,
) -> (
    Option<PendingMetadataAction>,
    Option<u64>,
    Option<(crate::app::types::ColumnKey, String)>,
    Option<MetadataEditorNavigation>,
) {
    let mut action = None;
    let mut open_hex = None;
    let mut add_list_column = None;
    let mut editor_navigation = None;
    egui::ScrollArea::vertical()
        .id_salt(("metadata_details", tab.tab_id))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(
                    &mut tab.metadata_detail_tab,
                    MetadataDetailTab::Properties,
                    "Properties",
                );
                ui.selectable_value(
                    &mut tab.metadata_detail_tab,
                    MetadataDetailTab::Decoded,
                    "Decoded",
                );
                ui.selectable_value(
                    &mut tab.metadata_detail_tab,
                    MetadataDetailTab::Text,
                    "Text",
                );
                ui.selectable_value(
                    &mut tab.metadata_detail_tab,
                    MetadataDetailTab::Waveform,
                    "Waveform",
                );
                ui.selectable_value(&mut tab.metadata_detail_tab, MetadataDetailTab::Hex, "Hex");
            });
            ui.separator();
            draw_detail_tab_content(
                ui,
                tab,
                document,
                node,
                waveform,
                play_fraction,
                hex_page,
                &mut open_hex,
            );
            if let Some(navigation) = metadata_editor_navigation(node) {
                ui.separator();
                let source_navigation_available =
                    !tab.dirty && same_file::is_same_file(&tab.path, source_path).unwrap_or(false);
                let (label, description) = match navigation {
                    MetadataEditorNavigation::Sample(sample) => (
                        format!("Jump to sample {sample}"),
                        "Open Wave and move the source playhead to this cue point.",
                    ),
                    MetadataEditorNavigation::Range {
                        start,
                        end_inclusive,
                    } => (
                        format!("Show loop {start}..{end_inclusive}"),
                        "Open Wave and select this smpl loop range.",
                    ),
                    MetadataEditorNavigation::Tempo(bpm) => (
                        format!("Use {bpm:.3} BPM"),
                        "Open Wave and copy this ACID tempo into the editor BPM grid.",
                    ),
                };
                let response = ui
                    .add_enabled(source_navigation_available, egui::Button::new(label))
                    .on_hover_text(if source_navigation_available {
                        description
                    } else {
                        "Unavailable because this view describes a virtual or edited source."
                    });
                if response.clicked() {
                    editor_navigation = Some(navigation);
                }
            }
            let sourced = document
                .normalized
                .iter()
                .filter(|field| {
                    field
                        .values
                        .iter()
                        .any(|value| value.source_node == node.id)
                })
                .collect::<Vec<_>>();
            if !sourced.is_empty() {
                ui.separator();
                ui.label(
                    RichText::new("NORMALIZED VALUES")
                        .small()
                        .strong()
                        .color(metadata_accent()),
                );
                for field in sourced {
                    let values = field
                        .values
                        .iter()
                        .filter(|value| value.source_node == node.id)
                        .map(|value| value.value.display())
                        .collect::<Vec<_>>()
                        .join("\n");
                    let accent = if field.conflict {
                        Color32::from_rgb(235, 185, 72)
                    } else {
                        metadata_accent()
                    };
                    draw_metadata_surface(ui, &field.key, accent, |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(values).color(metadata_value_text()))
                                .wrap()
                                .selectable(true),
                        );
                        if field.conflict {
                            ui.colored_label(
                                Color32::from_rgb(235, 185, 72),
                                "Conflicting source values",
                            );
                        }
                        if ui.small_button("Add as List Column").clicked() {
                            add_list_column = Some((
                                crate::app::types::ColumnKey::Normalized(field.key.clone()),
                                field.key.clone(),
                            ));
                        }
                    });
                    ui.add_space(4.0);
                }
            }
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                if ui.button("Open in Hex").clicked() {
                    open_hex = Some(node.payload.file_offset);
                }
                if ui.button("Add raw as List Column").clicked() {
                    add_list_column = Some((
                        crate::app::types::ColumnKey::Raw(crate::metadata::raw_column_key(
                            document.container,
                            &node.path,
                        )),
                        node.name.clone(),
                    ));
                }
                let busy = tab.metadata_action_rx.is_some();
                if ui
                    .add_enabled(!busy, egui::Button::new("Copy Hex"))
                    .clicked()
                {
                    action = Some(PendingMetadataAction::Copy {
                        payload: node.payload,
                    });
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("SHA-256"))
                    .clicked()
                {
                    action = Some(PendingMetadataAction::Hash {
                        payload: node.payload,
                    });
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("Extract..."))
                    .clicked()
                {
                    if let Some(output) = rfd::FileDialog::new()
                        .set_file_name(format!("{}.bin", node.name.trim()))
                        .save_file()
                    {
                        action = Some(PendingMetadataAction::Extract {
                            payload: node.payload,
                            // The native save dialog owns overwrite
                            // confirmation. Avoid another stat on the UI
                            // thread after it returns a user-controlled path.
                            overwrite: true,
                            output,
                        });
                    }
                }
            });
            ui.separator();
            ui.label("Search this payload");
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt(("metadata_search_kind", tab.tab_id))
                    .selected_text(
                        ["ASCII", "UTF-8", "Hex", "FourCC"][tab.metadata_search_kind.min(3)],
                    )
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut tab.metadata_search_kind, 0, "ASCII");
                        ui.selectable_value(&mut tab.metadata_search_kind, 1, "UTF-8");
                        ui.selectable_value(&mut tab.metadata_search_kind, 2, "Hex");
                        ui.selectable_value(&mut tab.metadata_search_kind, 3, "FourCC");
                    });
                ui.text_edit_singleline(&mut tab.metadata_search_query);
                if ui
                    .add_enabled(
                        tab.metadata_action_rx.is_none() && !tab.metadata_search_query.is_empty(),
                        egui::Button::new("Find"),
                    )
                    .clicked()
                {
                    action = Some(PendingMetadataAction::Search {
                        payload: node.payload,
                        kind: match tab.metadata_search_kind {
                            1 => SearchKind::Utf8,
                            2 => SearchKind::Hex,
                            3 => SearchKind::FourCc,
                            _ => SearchKind::Ascii,
                        },
                        query: tab.metadata_search_query.clone(),
                    });
                }
            });
            if let Some(status) = &tab.metadata_action_status {
                ui.add(egui::Label::new(RichText::new(status).monospace()).wrap());
            }
            if tab.metadata_action_rx.is_some() {
                let done = tab
                    .metadata_action_progress
                    .as_ref()
                    .map(|progress| progress.load(std::sync::atomic::Ordering::Relaxed))
                    .unwrap_or(0);
                let fraction = if tab.metadata_action_total == 0 {
                    0.0
                } else {
                    done as f32 / tab.metadata_action_total as f32
                };
                ui.add(
                    egui::ProgressBar::new(fraction.clamp(0.0, 1.0))
                        .text(format!("{} / {} bytes", done, tab.metadata_action_total)),
                );
                if ui.button("Cancel").clicked() {
                    if let Some(cancel) = &tab.metadata_action_cancel {
                        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
            for &offset in tab.metadata_search_results.iter().take(100) {
                if ui.link(format!("0x{offset:X} ({offset})")).clicked() {
                    open_hex = Some(offset);
                }
            }
        });
    (action, open_hex, add_list_column, editor_navigation)
}

fn metadata_editor_navigation(node: &MetadataNode) -> Option<MetadataEditorNavigation> {
    let summary = node.summary.as_deref()?.trim();
    if node.name.starts_with("Cue ") {
        return summary
            .strip_prefix("sample ")
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(MetadataEditorNavigation::Sample);
    }
    if node.name.starts_with("Loop ") {
        let (start, end) = summary.split_once("..")?;
        return Some(MetadataEditorNavigation::Range {
            start: start.trim().parse().ok()?,
            end_inclusive: end.trim().parse().ok()?,
        });
    }
    if node.name == "Tempo" {
        return summary
            .parse::<f32>()
            .ok()
            .filter(|bpm| bpm.is_finite() && *bpm > 0.0)
            .map(MetadataEditorNavigation::Tempo);
    }
    None
}

fn preferred_detail_tab(node: &MetadataNode) -> MetadataDetailTab {
    match node.content {
        ContentKind::Audio => MetadataDetailTab::Waveform,
        ContentKind::Text | ContentKind::Xml => MetadataDetailTab::Text,
        ContentKind::Image => MetadataDetailTab::Decoded,
        ContentKind::Binary => MetadataDetailTab::Hex,
        _ if node.summary.is_some() => MetadataDetailTab::Decoded,
        _ => MetadataDetailTab::Properties,
    }
}

fn draw_detail_tab_content(
    ui: &mut egui::Ui,
    tab: &mut crate::app::types::EditorTab,
    document: &MetadataDocument,
    node: &MetadataNode,
    waveform: &[(f32, f32)],
    play_fraction: Option<f32>,
    hex_page: Option<&MetadataHexPage>,
    open_hex: &mut Option<u64>,
) {
    match tab.metadata_detail_tab {
        MetadataDetailTab::Properties => {
            draw_metadata_value_card(
                ui,
                "NODE PATH",
                &node.path,
                Color32::from_rgb(98, 171, 235),
                false,
            );
            ui.add_space(6.0);
            let properties = [
                ("CONTENT", format!("{:?}", node.content)),
                ("PARSE STATUS", format!("{:?}", node.status)),
                (
                    "KNOWN STRUCTURE",
                    if node.known { "Yes" } else { "No" }.to_string(),
                ),
                (
                    "ABSOLUTE OFFSET",
                    format!("{}  ·  0x{:X}", node.offset, node.offset),
                ),
                ("HEADER SIZE", format!("{} bytes", node.header_size)),
                ("DECLARED SIZE", format!("{} bytes", node.declared_size)),
                ("READABLE SIZE", format!("{} bytes", node.readable_size)),
                (
                    "PAYLOAD RANGE",
                    format!(
                        "{} bytes  ·  starts at 0x{:X}",
                        node.payload.length, node.payload.file_offset
                    ),
                ),
            ];
            for row in properties.chunks(2) {
                ui.columns(2, |columns| {
                    for (index, (name, value)) in row.iter().enumerate() {
                        draw_metadata_value_card(
                            &mut columns[index],
                            name,
                            value,
                            metadata_accent(),
                            false,
                        );
                    }
                });
                ui.add_space(4.0);
            }
        }
        MetadataDetailTab::Decoded => {
            if let Some(summary) = &node.summary {
                draw_metadata_value_card(ui, "DECODED VALUE", summary, metadata_accent(), false);
            } else {
                draw_metadata_value_card(
                    ui,
                    "DECODED VALUE",
                    "No decoded value is available for this node.",
                    Color32::GRAY,
                    false,
                );
            }
            if node.content == ContentKind::Image {
                ui.separator();
                if let Some((_, texture)) = tab
                    .metadata_artwork_texture
                    .as_ref()
                    .filter(|(node_id, _)| *node_id == node.id)
                {
                    let source = texture.size_vec2();
                    let scale = (ui.available_width() / source.x.max(1.0)).min(1.0);
                    ui.add(egui::Image::new((texture.id(), source * scale)));
                } else {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Loading artwork preview...");
                    });
                }
            }
            let diagnostics = document
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.node == Some(node.id))
                .collect::<Vec<_>>();
            for diagnostic in diagnostics {
                ui.colored_label(
                    Color32::YELLOW,
                    format!("{}: {}", diagnostic.code, diagnostic.message),
                );
            }
        }
        MetadataDetailTab::Text => {
            ui.horizontal_wrapped(|ui| {
                ui.label("Encoding");
                egui::ComboBox::from_id_salt(("metadata_text_encoding", tab.tab_id))
                    .selected_text(
                        ["Auto", "UTF-8", "Shift_JIS", "Windows-1252"]
                            [tab.metadata_text_encoding.min(3)],
                    )
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut tab.metadata_text_encoding, 0, "Auto");
                        ui.selectable_value(&mut tab.metadata_text_encoding, 1, "UTF-8");
                        ui.selectable_value(&mut tab.metadata_text_encoding, 2, "Shift_JIS");
                        ui.selectable_value(&mut tab.metadata_text_encoding, 3, "Windows-1252");
                    });
                ui.label(RichText::new("Temporary display override").weak());
            });
            let raw = hex_page.and_then(|page| {
                let page_end = page.start.saturating_add(page.bytes.len() as u64);
                if node.payload.file_offset < page.start || node.payload.file_offset >= page_end {
                    return None;
                }
                let length = node
                    .payload
                    .length
                    .min(page_end.saturating_sub(node.payload.file_offset))
                    .min(64 * 1024);
                bytes_for_range(
                    page,
                    SourceRange {
                        offset: node.payload.file_offset,
                        length,
                    },
                )
            });
            let text = match (tab.metadata_text_encoding, raw) {
                (0, _) => node.summary.clone().unwrap_or_else(|| {
                    raw.map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                        .unwrap_or_default()
                }),
                (1, Some(bytes)) => String::from_utf8_lossy(bytes).into_owned(),
                (2, Some(bytes)) => encoding_rs::SHIFT_JIS.decode(bytes).0.into_owned(),
                (3, Some(bytes)) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
                (_, Some(bytes)) => String::from_utf8_lossy(bytes).into_owned(),
                (_, None) => String::new(),
            };
            if text.is_empty() {
                ui.label("Text bytes are loading or this node has no text representation.");
            } else {
                draw_metadata_surface(ui, "TEXT CONTENT", metadata_accent(), |ui| {
                    ui.add(
                        egui::Label::new(RichText::new(text).color(metadata_value_text()))
                            .wrap()
                            .selectable(true),
                    );
                });
                if raw.is_some_and(|bytes| (bytes.len() as u64) < node.payload.length) {
                    ui.label(
                        RichText::new("Preview is limited to the current 64 KiB page.").weak(),
                    );
                }
            }
        }
        MetadataDetailTab::Waveform => {
            if node.content == ContentKind::Audio {
                draw_inline_waveform(ui, waveform, play_fraction);
            } else {
                ui.label("This node is not an audio payload.");
            }
        }
        MetadataDetailTab::Hex => {
            if let Some(bytes) = hex_page.and_then(|page| {
                bytes_for_range(
                    page,
                    SourceRange {
                        offset: node.payload.file_offset,
                        length: node.payload.length.min(32),
                    },
                )
            }) {
                draw_binary_preview(ui, bytes, node.payload.file_offset, None);
            } else {
                ui.label("Hex bytes are loading...");
            }
            if ui.button("Open full Hex view").clicked() {
                *open_hex = Some(node.payload.file_offset);
            }
        }
    }
}

fn draw_live_pcm_status(
    ui: &mut egui::Ui,
    document: &MetadataDocument,
    exact_live: Option<(u64, SourceRange)>,
    source_time: Option<f64>,
    page: Option<&MetadataHexPage>,
) {
    if let (Some(mapping), Some((frame, range))) = (document.audio_mapping.as_ref(), exact_live) {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(Color32::LIGHT_GREEN, "Exact PCM source mapping");
            ui.monospace(format!(
                "frame {frame} · abs 0x{:X} · data+0x{:X} · {} bytes",
                range.offset,
                range.offset.saturating_sub(mapping.data_offset),
                range.length
            ));
        });
        if let Some(bytes) = page.and_then(|page| bytes_for_range(page, range)) {
            let bytes_per_channel = mapping.block_align as usize / mapping.channels.max(1) as usize;
            let padding_bits = mapping.container_bits.saturating_sub(mapping.valid_bits) as usize;
            let mut raw_layout = egui::text::LayoutJob::default();
            let font_id = egui::TextStyle::Monospace.resolve(ui.style());
            for channel in 0..mapping.channels as usize {
                let start = channel.saturating_mul(bytes_per_channel);
                let end = start.saturating_add(bytes_per_channel);
                let Some(sample_bytes) = bytes.get(start..end) else {
                    continue;
                };
                append_hex_text(
                    &mut raw_layout,
                    &format!("Ch{} ", channel + 1),
                    font_id.clone(),
                    Color32::TRANSPARENT,
                );
                for (byte_index, byte) in sample_bytes.iter().enumerate() {
                    for bit in (0..8).rev() {
                        let file_order_bit = byte_index.saturating_mul(8).saturating_add(bit);
                        let is_padding = file_order_bit < padding_bits;
                        append_bit_text(
                            &mut raw_layout,
                            if *byte & (1u8 << bit) == 0 { "0" } else { "1" },
                            font_id.clone(),
                            channel_color(channel),
                            is_padding,
                        );
                    }
                    if byte_index + 1 < sample_bytes.len() {
                        append_hex_text(
                            &mut raw_layout,
                            " ",
                            font_id.clone(),
                            Color32::TRANSPARENT,
                        );
                    }
                }
                if channel + 1 < mapping.channels as usize {
                    append_hex_text(
                        &mut raw_layout,
                        "   ",
                        font_id.clone(),
                        Color32::TRANSPARENT,
                    );
                }
            }
            ui.add(egui::Label::new(raw_layout).wrap());
            ui.horizontal_wrapped(|ui| {
                for channel in 0..mapping.channels as usize {
                    let start = channel.saturating_mul(bytes_per_channel);
                    let end = start.saturating_add(bytes_per_channel);
                    if let Some(sample) = bytes
                        .get(start..end)
                        .and_then(|sample| decode_wave_sample(mapping, sample))
                    {
                        ui.monospace(format!("Ch{} = {sample}", channel + 1));
                    }
                }
            });
        }
        ui.horizontal_wrapped(|ui| {
            for channel in 0..mapping.channels {
                if let Some(channel_range) = mapping.channel_range(frame, channel) {
                    ui.colored_label(
                        channel_color(channel as usize),
                        RichText::new(format!(
                            "Ch{} 0x{:X}..0x{:X}",
                            channel + 1,
                            channel_range.offset,
                            channel_range.offset + channel_range.length
                        ))
                        .monospace(),
                    );
                }
            }
            ui.colored_label(
                Color32::from_rgb(235, 185, 72),
                format!(
                    "{} valid bits + {} padding bits in {} container bits",
                    mapping.valid_bits,
                    mapping.container_bits.saturating_sub(mapping.valid_bits),
                    mapping.container_bits
                ),
            );
        });
    } else if document.audio_mapping.is_some() {
        if source_time.is_some() {
            ui.colored_label(
                Color32::YELLOW,
                "Source-time only: an edit, preview, virtual source, or different playback source prevents exact raw-bit mapping.",
            );
        } else {
            ui.label(
                RichText::new(
                    "Exact PCM source mapping is available; play this editor source to follow raw frames.",
                )
                .weak(),
            );
        }
    } else {
        ui.horizontal_wrapped(|ui| {
            if let Some(time) = source_time {
                ui.monospace(format!("Source time {time:.6} s"));
            }
            ui.colored_label(
                Color32::GRAY,
                "Compressed container: exact playback-to-bitstream mapping is unavailable.",
            );
        });
    }
}

fn append_hex_text(
    layout: &mut egui::text::LayoutJob,
    text: &str,
    font_id: egui::FontId,
    background: Color32,
) {
    layout.append(
        text,
        0.0,
        egui::TextFormat {
            font_id,
            color: Color32::from_rgb(224, 229, 235),
            background,
            ..Default::default()
        },
    );
}

fn append_bit_text(
    layout: &mut egui::text::LayoutJob,
    text: &str,
    font_id: egui::FontId,
    channel: Color32,
    padding: bool,
) {
    layout.append(
        text,
        0.0,
        egui::TextFormat {
            font_id,
            color: if padding {
                Color32::from_rgb(38, 31, 15)
            } else {
                Color32::WHITE
            },
            background: if padding {
                Color32::from_rgb(235, 185, 72)
            } else {
                channel
            },
            ..Default::default()
        },
    );
}

fn channel_color(channel: usize) -> Color32 {
    const COLORS: [Color32; 8] = [
        Color32::from_rgb(35, 113, 91),
        Color32::from_rgb(46, 89, 142),
        Color32::from_rgb(111, 67, 145),
        Color32::from_rgb(151, 76, 70),
        Color32::from_rgb(135, 103, 38),
        Color32::from_rgb(42, 113, 128),
        Color32::from_rgb(122, 71, 109),
        Color32::from_rgb(80, 104, 58),
    ];
    COLORS[channel % COLORS.len()]
}

fn decode_wave_sample(mapping: &crate::metadata::AudioByteMapping, bytes: &[u8]) -> Option<String> {
    let effective_tag = if mapping.format_tag == 0xfffe {
        mapping.format_subtype?
    } else {
        mapping.format_tag
    };
    if effective_tag == 3 {
        return match bytes {
            [a, b, c, d] => Some(format!("{:.9}", f32::from_le_bytes([*a, *b, *c, *d]))),
            [a, b, c, d, e, f, g, h] => Some(format!(
                "{:.17}",
                f64::from_le_bytes([*a, *b, *c, *d, *e, *f, *g, *h])
            )),
            _ => None,
        };
    }
    if effective_tag != 1 || bytes.is_empty() || bytes.len() > 8 {
        return None;
    }
    if mapping.container_bits == 8 {
        return bytes.first().map(|value| value.to_string());
    }
    let mut raw = 0u64;
    for (shift, byte) in bytes.iter().enumerate() {
        raw |= (*byte as u64) << (shift * 8);
    }
    let container_bits = mapping.container_bits.min(64).max(1);
    let valid_bits = mapping.valid_bits.min(container_bits).max(1);
    let padding = container_bits.saturating_sub(valid_bits);
    let valid_raw = raw >> padding;
    let sign_bit = 1u64 << (valid_bits - 1);
    let signed = if valid_raw & sign_bit != 0 {
        (valid_raw as i128) - (1i128 << valid_bits)
    } else {
        valid_raw as i128
    };
    Some(format!(
        "{signed} (0x{raw:0width$X})",
        width = bytes.len() * 2
    ))
}

fn bytes_for_range(page: &MetadataHexPage, range: SourceRange) -> Option<&[u8]> {
    let start = range.offset.checked_sub(page.start)? as usize;
    let end = start.checked_add(range.length as usize)?;
    page.bytes.get(start..end)
}

fn ranges_overlap(a: SourceRange, b: SourceRange) -> bool {
    a.offset < b.offset.saturating_add(b.length) && b.offset < a.offset.saturating_add(a.length)
}

fn metadata_node_hidden_in_structure(
    container: crate::metadata::ContainerKind,
    node: &MetadataNode,
) -> bool {
    matches!(
        container,
        crate::metadata::ContainerKind::RiffWave
            | crate::metadata::ContainerKind::Rf64
            | crate::metadata::ContainerKind::Bw64
    ) && node.name.trim_end() == "fmt"
}

fn visible_metadata_structure_target(
    document: &MetadataDocument,
    mut node_id: NodeId,
) -> Option<NodeId> {
    loop {
        let node = document.node(node_id)?;
        if !metadata_node_hidden_in_structure(document.container, node) {
            return Some(node_id);
        }
        node_id = node.parent?;
    }
}

fn centered_hex_row_scroll_offset(
    row: usize,
    row_count: usize,
    row_extent: f32,
    viewport_height: f32,
) -> f32 {
    let content_height = row_count as f32 * row_extent;
    let row_center = (row as f32 + 0.5) * row_extent;
    let centered = row_center - viewport_height * 0.5;
    centered.clamp(0.0, (content_height - viewport_height).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar_node(name: &str, summary: &str) -> MetadataNode {
        MetadataNode {
            id: 0,
            parent: None,
            children: Vec::new(),
            name: name.to_owned(),
            path: format!("/test/{name}"),
            offset: 0,
            header_size: 0,
            declared_size: 0,
            readable_size: 0,
            payload: PayloadRef {
                file_offset: 0,
                length: 0,
            },
            content: ContentKind::Text,
            known: true,
            status: crate::metadata::ParseStatus::Parsed,
            summary: Some(summary.to_owned()),
        }
    }

    #[test]
    fn metadata_navigation_decodes_cue_loop_and_tempo_nodes() {
        assert_eq!(
            metadata_editor_navigation(&scalar_node("Cue 7", "sample 48000")),
            Some(MetadataEditorNavigation::Sample(48_000))
        );
        assert_eq!(
            metadata_editor_navigation(&scalar_node("Loop 1", "120..960")),
            Some(MetadataEditorNavigation::Range {
                start: 120,
                end_inclusive: 960,
            })
        );
        assert_eq!(
            metadata_editor_navigation(&scalar_node("Tempo", "128.500")),
            Some(MetadataEditorNavigation::Tempo(128.5))
        );
    }

    #[test]
    fn centered_hex_scroll_keeps_live_row_near_viewport_center() {
        let offset = centered_hex_row_scroll_offset(2_052, 4_096, 22.0, 440.0);
        let row_center_in_view = (2_052.5 * 22.0) - offset;
        assert!((row_center_in_view - 220.0).abs() < f32::EPSILON);
        assert_eq!(centered_hex_row_scroll_offset(0, 4_096, 22.0, 440.0), 0.0);
    }

    #[test]
    fn riff_fmt_node_is_hidden_only_from_the_structure_list() {
        let fmt = scalar_node("fmt ", "48000 Hz");
        assert!(metadata_node_hidden_in_structure(
            crate::metadata::ContainerKind::RiffWave,
            &fmt
        ));
        assert!(!metadata_node_hidden_in_structure(
            crate::metadata::ContainerKind::Aiff,
            &fmt
        ));
        assert!(!metadata_node_hidden_in_structure(
            crate::metadata::ContainerKind::RiffWave,
            &scalar_node("data", "16 bytes")
        ));
    }

    #[test]
    fn audio_timeline_maps_disjoint_payload_ranges_without_metadata_gaps() {
        let timeline = MetadataAudioTimeline {
            ranges: vec![
                SourceRange {
                    offset: 100,
                    length: 200,
                },
                SourceRange {
                    offset: 500,
                    length: 100,
                },
            ],
            total_bytes: 300,
        };
        assert_eq!(timeline.file_offset_for_fraction(0.0), Some(100));
        assert_eq!(timeline.file_offset_for_fraction(0.5), Some(250));
        assert_eq!(timeline.file_offset_for_fraction(1.0), Some(599));
        assert!(timeline
            .fraction_span(SourceRange {
                offset: 0,
                length: 64,
            })
            .is_none());
        let (start, end) = timeline
            .fraction_span(SourceRange {
                offset: 500,
                length: 30,
            })
            .expect("second audio payload span");
        assert!((start - 2.0 / 3.0).abs() < f64::EPSILON);
        assert!((end - 0.766_666_666_666_666_7).abs() < f64::EPSILON);
    }

    #[test]
    fn pcm_waveform_fraction_maps_to_the_complete_interleaved_frame() {
        let mapping = crate::metadata::AudioByteMapping {
            node_id: 0,
            data_offset: 1_000,
            data_length: 400,
            sample_rate: 100,
            channels: 2,
            block_align: 4,
            container_bits: 16,
            valid_bits: 16,
            format_tag: 1,
            format_subtype: None,
            format_name: "PCM".to_string(),
        };
        let document = MetadataDocument {
            schema_version: 1,
            source: PathBuf::from("pcm.wav"),
            container: crate::metadata::ContainerKind::RiffWave,
            file_len: 1_400,
            roots: Vec::new(),
            nodes: Vec::new(),
            normalized: Vec::new(),
            diagnostics: Vec::new(),
            completion: crate::metadata::Completion::Complete,
            audio_mapping: Some(mapping),
        };
        let timeline = MetadataAudioTimeline::from_document(&document);

        assert_eq!(
            metadata_audio_range_for_fraction(&document, &timeline, 0.5),
            Some(SourceRange {
                offset: 1_200,
                length: 4,
            })
        );
        let row_fraction = timeline
            .fraction_for_range(SourceRange {
                offset: 1_200,
                length: 16,
            })
            .expect("audio row must map to the waveform");
        assert!((row_fraction - 0.52).abs() < f64::EPSILON);
        assert_eq!(
            metadata_audio_range_for_fraction(&document, &timeline, row_fraction),
            Some(SourceRange {
                offset: 1_208,
                length: 4,
            })
        );
    }
}
