use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use egui::{Align, Color32, FontData, FontDefinitions, FontFamily, FontId, Key, RichText, Sense, TextStyle, Visuals};
use egui_extras::TableBuilder;
use crate::audio::AudioEngine;
use crate::wave::{build_minmax, decode_wav_mono, decode_wav_multi, prepare_for_speed, process_pitchshift_offline, process_timestretch_offline, resample_linear};
use walkdir::WalkDir;

mod types;
mod helpers;
mod meta;
mod logic;
use self::{types::*, helpers::*, meta::spawn_meta_worker};

// moved to types.rs

pub struct WavesPreviewer {
    pub audio: AudioEngine,
    pub root: Option<PathBuf>,
    pub files: Vec<PathBuf>,
    pub all_files: Vec<PathBuf>,
    pub selected: Option<usize>,
    pub volume_db: f32,
    pub playback_rate: f32,
    // unified numeric control via DragValue; no string normalization
    pub pitch_semitones: f32,
    pub meter_db: f32,
    pub tabs: Vec<EditorTab>,
    pub active_tab: Option<usize>,
    pub meta: HashMap<PathBuf, FileMeta>,
    pub meta_rx: Option<std::sync::mpsc::Receiver<(PathBuf, FileMeta)>>,
    // dynamic row height for wave thumbnails (list view)
    pub wave_row_h: f32,
    // sorting
    sort_key: SortKey,
    sort_dir: SortDir,
    // scroll behavior
    scroll_to_selected: bool,
    // original order snapshot for tri-state sort
    original_files: Vec<PathBuf>,
    // search
    search_query: String,
    // processing mode
    mode: RateMode,
    // heavy processing state (overlay)
    processing: Option<ProcessingState>,
}

// moved to types.rs

impl WavesPreviewer {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<Self> {
        // Visuals (dark, chic) + fonts
        let mut visuals = Visuals::dark();
        visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(20, 20, 23);
        visuals.widgets.inactive.bg_fill = Color32::from_rgb(28, 28, 32);
        // Remove hover brightening to avoid sluggish tracking effect
        visuals.widgets.hovered = visuals.widgets.inactive.clone();
        visuals.widgets.active = visuals.widgets.inactive.clone();
        visuals.panel_fill = Color32::from_rgb(18, 18, 20);
        cc.egui_ctx.set_visuals(visuals);
        let mut fonts = FontDefinitions::default();
        let candidates = [
            "C:/Windows/Fonts/meiryo.ttc",
            "C:/Windows/Fonts/YuGothM.ttc",
            "C:/Windows/Fonts/msgothic.ttc",
        ];
        for p in candidates {
            if let Ok(bytes) = std::fs::read(p) {
                fonts.font_data.insert("jp".into(), FontData::from_owned(bytes));
                fonts.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "jp".into());
                fonts.families.get_mut(&FontFamily::Monospace).unwrap().insert(0, "jp".into());
                break;
            }
        }
        cc.egui_ctx.set_fonts(fonts);
        let mut style = (*cc.egui_ctx.style()).clone();
        style.text_styles.insert(TextStyle::Body, FontId::proportional(16.0));
        style.text_styles.insert(TextStyle::Monospace, FontId::monospace(14.0));
        cc.egui_ctx.set_style(style);

        let audio = AudioEngine::new()?;
        // 初期状態（リスト表示）ではループを無効にする
        audio.set_loop_enabled(false);
        Ok(Self {
            audio,
            root: None,
            files: Vec::new(),
            all_files: Vec::new(),
            selected: None,
            volume_db: -12.0,
            playback_rate: 1.0,
            pitch_semitones: 0.0,
            meter_db: -80.0,
            tabs: Vec::new(),
            active_tab: None,
            meta: HashMap::new(),
            meta_rx: None,
            wave_row_h: 26.0,
            sort_key: SortKey::File,
            sort_dir: SortDir::None,
            scroll_to_selected: false,
            original_files: Vec::new(),
            search_query: String::new(),
            mode: RateMode::Speed,
            processing: None,
        })
    }

}

impl eframe::App for WavesPreviewer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drain metadata updates
        if let Some(rx) = &self.meta_rx {
            let mut resort = false;
            while let Ok((p, m)) = rx.try_recv() { self.meta.insert(p, m); resort = true; }
            if resort { self.apply_sort(); ctx.request_repaint(); }
        }

        // Drain heavy processing result
        if let Some(state) = &self.processing {
            if let Ok(res) = state.rx.try_recv() {
                // Apply new buffer and waveform
                self.audio.set_samples(std::sync::Arc::new(res.samples));
                self.audio.stop();
                if let Some(idx) = self.tabs.iter().position(|t| t.path == res.path) {
                    if let Some(tab) = self.tabs.get_mut(idx) { tab.waveform_minmax = res.waveform; }
                }
                // full-buffer loop region if needed
                if let Some(buf) = self.audio.shared.samples.load().as_ref() { self.audio.set_loop_region(0, buf.len()); }
                self.processing = None;
                ctx.request_repaint();
            }
        }

        // Shortcuts
        if ctx.input(|i| i.key_pressed(Key::Space)) { self.audio.toggle_play(); }
        
        // Ctrl+W でアクティブタブを閉じる
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(Key::W)) {
            if let Some(active_idx) = self.active_tab {
                self.audio.stop();
                self.tabs.remove(active_idx);
                // 閉じたタブの後にタブがあれば次のタブ、なければ前のタブをアクティブに
                if !self.tabs.is_empty() {
                    let new_active = if active_idx < self.tabs.len() { 
                        active_idx 
                    } else { 
                        self.tabs.len() - 1 
                    };
                    self.active_tab = Some(new_active);
                } else {
                    self.active_tab = None;
                }
            }
        }
        
        if let Some(tab_idx) = self.active_tab {
            if ctx.input(|i| i.key_pressed(Key::L)) {
                let tab = &mut self.tabs[tab_idx];
                tab.loop_enabled = !tab.loop_enabled;
                self.audio.set_loop_enabled(tab.loop_enabled);
                if let Some(buf) = self.audio.shared.samples.load().as_ref() { self.audio.set_loop_region(0, buf.len()); }
            }
        }
        if self.active_tab.is_none() {
            let mut changed = false;
            let len = self.files.len();
            if len > 0 {
                if ctx.input(|i| i.key_pressed(Key::ArrowDown)) { let next = match self.selected { Some(i) => (i+1).min(len-1), None => 0 }; self.selected = Some(next); changed = true; self.scroll_to_selected = true; }
                if ctx.input(|i| i.key_pressed(Key::ArrowUp)) { let prev = match self.selected { Some(i) if i>0 => i-1, _ => 0 }; self.selected = Some(prev); changed = true; self.scroll_to_selected = true; }
                if ctx.input(|i| i.key_pressed(Key::Enter)) { if let Some(i) = self.selected { let p_owned = self.files.get(i).cloned(); if let Some(p) = p_owned.as_ref() { self.open_or_activate_tab(p); } } }
                if changed {
                    if let Some(i) = self.selected {
                        let p_owned = self.files.get(i).cloned();
                        if let Some(p) = p_owned.as_ref() {
                            // リスト表示時は常にループを無効にする
                            self.audio.set_loop_enabled(false);
                            match self.mode {
                                RateMode::Speed => { let _ = prepare_for_speed(p, &self.audio, &mut Vec::new(), self.playback_rate); self.audio.set_rate(self.playback_rate); }
                                _ => { self.audio.set_rate(1.0); self.spawn_heavy_processing(p); }
                            }
                        }
                    }
                }
            }
        }

        // Meter
        let rms = self.audio.shared.meter_rms.load(std::sync::atomic::Ordering::Relaxed).max(1e-9);
        self.meter_db = 20.0 * rms.log10();

        // Drag & drop: add files/folders
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if !dropped.is_empty() {
            let mut files: Vec<PathBuf> = Vec::new();
            let mut dirs: Vec<PathBuf> = Vec::new();
            for f in dropped {
                if let Some(p) = f.path.clone() {
                    if p.is_dir() { dirs.push(p); } else { files.push(p); }
                }
            }
            let mut added = 0usize;
            if !files.is_empty() { added += self.add_files_merge(&files); }
            for d in dirs { added += self.add_folder_merge(&d); }
            if added > 0 { self.after_add_refresh(); }
        }

        // Top controls (wrap for small width)
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.menu_button("Choose", |ui| {
                    if ui.button("Folder...").clicked() {
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() { self.root = Some(dir); self.rescan(); }
                        ui.close_menu();
                    }
                    if ui.button("Files...").clicked() {
                        if let Some(files) = rfd::FileDialog::new().add_filter("WAV", &["wav"]).pick_files() {
                            self.replace_with_files(&files);
                            self.after_add_refresh();
                        }
                        ui.close_menu();
                    }
                });
                // Files total + loading indicator
                let total_vis = self.files.len();
                let total_all = self.all_files.len();
                if total_all > 0 {
                    let loading = self.meta.len() < total_all || self.meta.values().any(|m| m.rms_db.is_none() || m.thumb.is_empty());
                    let label = if self.search_query.is_empty() {
                        if loading { format!("Files: {} ⏳", total_all) } else { format!("Files: {}", total_all) }
                    } else {
                        if loading { format!("Files: {} / {} ⏳", total_vis, total_all) } else { format!("Files: {} / {}", total_vis, total_all) }
                    };
                    ui.label(RichText::new(label).monospace());
                }
                ui.separator();
                ui.label("Volume (dB)");
                if ui.add(egui::Slider::new(&mut self.volume_db, -80.0..=6.0)).changed() { self.audio.set_volume(db_to_amp(self.volume_db)); }
                ui.separator();
                // Mode: segmented + compact numeric control (DragValue)
                ui.scope(|ui| {
                    let s = ui.style_mut();
                    s.spacing.item_spacing.x = 6.0;
                    s.spacing.button_padding = egui::vec2(4.0, 2.0);
                    ui.label("Mode");
                    let prev_mode = self.mode;
                    for (m, label) in [(RateMode::Speed, "Speed"), (RateMode::PitchShift, "Pitch"), (RateMode::TimeStretch, "Stretch")] {
                        if ui.selectable_label(self.mode == m, label).clicked() { self.mode = m; }
                    }
                    if self.mode != prev_mode {
                        match self.mode {
                            RateMode::Speed => { self.audio.set_rate(self.playback_rate); }
                            _ => { self.audio.set_rate(1.0); self.rebuild_current_buffer_with_mode(); }
                        }
                    }
                    match self.mode {
                        RateMode::Speed => {
                            let resp = ui.add(
                                egui::DragValue::new(&mut self.playback_rate)
                                    .clamp_range(0.25..=4.0)
                                    .speed(0.05)
                                    .fixed_decimals(2)
                                    .suffix(" x")
                            );
                            if resp.changed() { self.audio.set_rate(self.playback_rate); }
                        }
                        RateMode::PitchShift => {
                            let resp = ui.add(
                                egui::DragValue::new(&mut self.pitch_semitones)
                                    .clamp_range(-12.0..=12.0)
                                    .speed(0.1)
                                    .fixed_decimals(1)
                                    .suffix(" st")
                            );
                            if resp.changed() { self.audio.set_rate(1.0); self.rebuild_current_buffer_with_mode(); }
                        }
                        RateMode::TimeStretch => {
                            let resp = ui.add(
                                egui::DragValue::new(&mut self.playback_rate)
                                    .clamp_range(0.25..=4.0)
                                    .speed(0.05)
                                    .fixed_decimals(2)
                                    .suffix(" x")
                            );
                            if resp.changed() { self.audio.set_rate(1.0); self.rebuild_current_buffer_with_mode(); }
                        }
                    }
                });
                ui.separator();
                let play_text = if self.audio.shared.playing.load(std::sync::atomic::Ordering::Relaxed) { "Pause (Space)" } else { "Play (Space)" };
                if ui.button(play_text).clicked() { self.audio.toggle_play(); }
                ui.separator();
                // Search bar
                let te = egui::TextEdit::singleline(&mut self.search_query).hint_text("Search...");
                if ui.add(te).changed() { self.apply_filter_from_search(); self.apply_sort(); }
                if !self.search_query.is_empty() {
                    if ui.button("x").on_hover_text("Clear").clicked() { self.search_query.clear(); self.apply_filter_from_search(); self.apply_sort(); }
                }
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    let db = self.meter_db; let bar_w = 200.0; let bar_h = 16.0;
                    let (rect, painter) = ui.allocate_painter(egui::vec2(bar_w, bar_h), Sense::hover());
                    painter.rect_stroke(rect.rect, 2.0, egui::Stroke::new(1.0, Color32::GRAY));
                    let norm = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
                    let fill = egui::Rect::from_min_size(rect.rect.min, egui::vec2(bar_w * norm, bar_h));
                    painter.rect_filled(fill, 0.0, Color32::from_rgb(100, 220, 120));
                    ui.label(RichText::new(format!("{db:.1} dBFS")).monospace());
                });
            });
        });

        let mut activate_path: Option<PathBuf> = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            // Tabs
            ui.horizontal_wrapped(|ui| {
                let is_list = self.active_tab.is_none();
                let list_label = if is_list { RichText::new("[List]").strong() } else { RichText::new("List") };
                if ui.selectable_label(is_list, list_label).clicked() { 
                    self.active_tab = None; 
                    // タブ切り替え時に音声を停止
                    self.audio.stop();
                    // リスト表示時は常にループを無効にする
                    self.audio.set_loop_enabled(false);
                }
                let mut to_close: Option<usize> = None;
                for (i, tab) in self.tabs.iter().enumerate() {
                    let active = self.active_tab == Some(i);
                    let text = if active { RichText::new(format!("[{}]", tab.display_name)).strong() } else { RichText::new(tab.display_name.clone()) };
                    ui.horizontal(|ui| {
                        if ui.selectable_label(active, text).clicked() { 
                            self.active_tab = Some(i); 
                            activate_path = Some(tab.path.clone()); 
                            // タブ切り替え時に音声を停止
                            self.audio.stop();
                        }
                        if ui.button("x").on_hover_text("Close").clicked() { 
                            to_close = Some(i); 
                            // タブ閉じる時に音声を停止
                            self.audio.stop();
                        }
                    });
                }
                if let Some(i) = to_close { self.tabs.remove(i); match self.active_tab { Some(ai) if ai==i => self.active_tab=None, Some(ai) if ai>i => self.active_tab=Some(ai-1), _=>{} } }
            });
            ui.separator();

            if let Some(tab_idx) = self.active_tab {
                // Editor view
                let tab = &mut self.tabs[tab_idx];
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tab.path.display().to_string()).monospace());
                    ui.separator();
                    if ui.selectable_label(tab.loop_enabled, if tab.loop_enabled { "Loop: On" } else { "Loop: Off" }).clicked() {
                        tab.loop_enabled = !tab.loop_enabled;
                        self.audio.set_loop_enabled(tab.loop_enabled);
                        // full-buffer loop
                        if let Some(buf) = self.audio.shared.samples.load().as_ref() { self.audio.set_loop_region(0, buf.len()); }
                    }
                });
                ui.separator();

                let avail = ui.available_size();
                // allocate editor canvas
                let canvas_h = (avail.x * 0.35).clamp(180.0, avail.y);
                let (resp, painter) = ui.allocate_painter(egui::vec2(avail.x, canvas_h), Sense::click_and_drag());
                let rect = resp.rect;
                let w = rect.width().max(1.0); let h = rect.height().max(1.0);
                painter.rect_filled(rect, 0.0, Color32::from_rgb(16,16,18));

                // Layout parameters
                let gutter_w = 44.0;
                let wave_left = rect.left() + gutter_w;
                let wave_w = (w - gutter_w).max(1.0);
                let ch_n = tab.ch_samples.len().max(1);
                let lane_h = h / ch_n as f32;

                // Initialize zoom to fit if unset
                if tab.samples_len > 0 && tab.samples_per_px <= 0.0 {
                    tab.samples_per_px = (tab.samples_len as f32 / wave_w).max(1.0);
                    tab.view_offset = 0;
                }

                // Handle interactions (seek, zoom, pan)
                if resp.hovered() {
                    // Zoom with Ctrl + wheel (use hovered pos over this widget)
                    let wheel = ui.input(|i| i.raw_scroll_delta);
                    let scroll_y = wheel.y;
                    let modifiers = ui.input(|i| i.modifiers);
                    let pointer_pos = resp.hover_pos();
                    if modifiers.ctrl && scroll_y.abs() > 0.0 && tab.samples_len > 0 {
                        let factor = if scroll_y > 0.0 { 0.9 } else { 1.1 };
                        let old_spp = tab.samples_per_px.max(0.0001);
                        let cursor_x = pointer_pos.map(|p| p.x).unwrap_or(wave_left + wave_w * 0.5).clamp(wave_left, wave_left + wave_w);
                        let t = ((cursor_x - wave_left) / wave_w).clamp(0.0, 1.0);
                        let vis = (wave_w * old_spp).ceil() as usize;
                        let anchor = tab.view_offset.saturating_add((t * vis as f32) as usize).min(tab.samples_len);
                        tab.samples_per_px = (old_spp * factor).clamp(0.1, 64.0);
                        let vis2 = (wave_w * tab.samples_per_px).ceil() as usize;
                        let left = anchor.saturating_sub((t * vis2 as f32) as usize);
                        let max_left = tab.samples_len.saturating_sub(vis2);
                        tab.view_offset = left.min(max_left);
                    }
                    // Pan with Shift + wheel
                    // Prefer horizontal wheel for pan if available; fall back to vertical
                    let scroll_for_pan = if wheel.x.abs() > 0.0 { wheel.x } else { wheel.y };
                    if modifiers.shift && scroll_for_pan.abs() > 0.0 && tab.samples_len > 0 {
                        let delta_px = -scroll_for_pan.signum() * 60.0; // a page step
                        let delta = (delta_px * tab.samples_per_px) as isize;
                        let mut off = tab.view_offset as isize + delta;
                        let vis = (wave_w * tab.samples_per_px).ceil() as usize;
                        let max_left = tab.samples_len.saturating_sub(vis);
                        if off < 0 { off = 0; }
                        if off as usize > max_left { off = max_left as isize; }
                        tab.view_offset = off as usize;
                    }
                }
                // Seek by click/drag (primary button)
                if resp.clicked() || resp.dragged() {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let x = pos.x.clamp(wave_left, wave_left + wave_w);
                        let t = ((x - wave_left) / wave_w).clamp(0.0, 1.0);
                        let vis = (wave_w * tab.samples_per_px.max(0.0001)).ceil() as usize;
                        let seek = tab.view_offset.saturating_add((t * vis as f32) as usize).min(tab.samples_len);
                        self.audio.seek_to_sample(seek);
                    }
                }

                // Draw per-channel lanes with dB grid and playhead
                for (ci, ch) in tab.ch_samples.iter().enumerate() {
                    let lane_top = rect.top() + lane_h * ci as f32;
                    let lane_rect = egui::Rect::from_min_size(egui::pos2(wave_left, lane_top), egui::vec2(wave_w, lane_h));
                    // dB lines: -6, -12 dBFS and center line (0 amp)
                    let dbs = [-6.0f32, -12.0f32];
                    // center
                    painter.line_segment([egui::pos2(lane_rect.left(), lane_rect.center().y), egui::pos2(lane_rect.right(), lane_rect.center().y)], egui::Stroke::new(1.0, Color32::from_rgb(45,45,50)));
                    for &db in &dbs {
                        let a = db_to_amp(db).clamp(0.0, 1.0);
                        let y0 = lane_rect.center().y - a * (lane_rect.height()*0.48);
                        let y1 = lane_rect.center().y + a * (lane_rect.height()*0.48);
                        painter.line_segment([egui::pos2(lane_rect.left(), y0), egui::pos2(lane_rect.right(), y0)], egui::Stroke::new(1.0, Color32::from_rgb(45,45,50)));
                        painter.line_segment([egui::pos2(lane_rect.left(), y1), egui::pos2(lane_rect.right(), y1)], egui::Stroke::new(1.0, Color32::from_rgb(45,45,50)));
                        // labels on the left gutter
                        let fid = TextStyle::Monospace.resolve(ui.style());
                        painter.text(egui::pos2(rect.left() + 2.0, y0), egui::Align2::LEFT_CENTER, format!("{db:.0} dB"), fid, Color32::GRAY);
                    }

                    // visible range
                    let spp = tab.samples_per_px.max(0.0001);
                    let vis = (wave_w * spp).ceil() as usize;
                    let start = tab.view_offset.min(tab.samples_len);
                    let end = (start + vis).min(tab.samples_len);
                    let bins = wave_w as usize;
                    if bins > 0 && end > start {
                        let mut tmp = Vec::new();
                        build_minmax(&mut tmp, &ch[start..end], bins);
                        let n = tmp.len().max(1) as f32;
                        for (idx, &(mn, mx)) in tmp.iter().enumerate() {
                            let x = lane_rect.left() + (idx as f32 / n) * wave_w;
                            let y0 = lane_rect.center().y - mx * (lane_rect.height()*0.48);
                            let y1 = lane_rect.center().y - mn * (lane_rect.height()*0.48);
                            let amp = (mn.abs().max(mx.abs())).clamp(0.0, 1.0);
                            let col = amp_to_color(amp);
                            painter.line_segment([egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))], egui::Stroke::new(1.0, col));
                        }
                    }
                }

                // Shared playhead across lanes
                if tab.samples_len > 0 {
                    if let Some(buf) = self.audio.shared.samples.load().as_ref() {
                        let len = buf.len().max(1);
                        let pos = self.audio.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed).min(len);
                        let spp = tab.samples_per_px.max(0.0001);
                        let x = wave_left + ((pos.saturating_sub(tab.view_offset)) as f32 / spp).clamp(0.0, wave_w);
                        painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(2.0, Color32::from_rgb(70,140,255)));
                    }
                }
            } else {
                // List view
                let mut to_open: Option<PathBuf> = None;
                let text_height = egui::TextStyle::Body.resolve(ui.style()).size;
                let header_h = text_height * 1.6; let row_h = self.wave_row_h.max(text_height * 1.3);
                let avail_h = ui.available_height();
                // Build table directly; size the scrolled body to fill remaining height
                // Also expand to full width so the scroll bar is at the right edge
                ui.set_min_width(ui.available_width());
                let mut sort_changed = false;
                let table = TableBuilder::new(ui)
                    .striped(true)
                    .resizable(true)
                    .sense(egui::Sense::click())
                    .cell_layout(egui::Layout::left_to_right(Align::Center))
                    .column(egui_extras::Column::initial(200.0).resizable(true))     // File (resizable)
                    .column(egui_extras::Column::initial(250.0).resizable(true))     // Folder (resizable)
                    .column(egui_extras::Column::initial(60.0).resizable(true))      // Length (resizable)
                    .column(egui_extras::Column::initial(40.0).resizable(true))      // Ch (resizable)
                    .column(egui_extras::Column::initial(70.0).resizable(true))      // SampleRate (resizable)
                    .column(egui_extras::Column::initial(50.0).resizable(true))      // Bits (resizable)
                    .column(egui_extras::Column::initial(90.0).resizable(true))      // Level (resizable)
                    .column(egui_extras::Column::initial(150.0).resizable(true))     // Wave (resizable)
                    .column(egui_extras::Column::remainder())                        // Spacer (fills remainder)
                    .min_scrolled_height((avail_h - header_h).max(0.0));

                table.header(header_h, |mut header| {
                    header.col(|ui| { sort_changed |= sortable_header(ui, "File", &mut self.sort_key, &mut self.sort_dir, SortKey::File, true); });
                    header.col(|ui| { sort_changed |= sortable_header(ui, "Folder", &mut self.sort_key, &mut self.sort_dir, SortKey::Folder, true); });
                    header.col(|ui| { sort_changed |= sortable_header(ui, "Length", &mut self.sort_key, &mut self.sort_dir, SortKey::Length, true); });
                    header.col(|ui| { sort_changed |= sortable_header(ui, "Ch", &mut self.sort_key, &mut self.sort_dir, SortKey::Channels, true); });
                    header.col(|ui| { sort_changed |= sortable_header(ui, "SR", &mut self.sort_key, &mut self.sort_dir, SortKey::SampleRate, true); });
                    header.col(|ui| { sort_changed |= sortable_header(ui, "Bits", &mut self.sort_key, &mut self.sort_dir, SortKey::Bits, true); });
                    header.col(|ui| { sort_changed |= sortable_header(ui, "Level (dBFS)", &mut self.sort_key, &mut self.sort_dir, SortKey::Level, false); });
                    header.col(|ui| { ui.label(RichText::new("Wave").strong()); });
                    header.col(|_ui| { /* spacer */ });
                }).body(|body| {
                    let data_len = self.files.len();
                    // Ensure the table body fills the remaining height
                    let min_rows_for_height = ((avail_h - header_h).max(0.0) / row_h).ceil() as usize;
                    let total_rows = data_len.max(min_rows_for_height);

                    // Use virtualized rows for performance with large lists
                    body.rows(row_h, total_rows, |mut row| {
                        let row_idx = row.index();
                        let is_data = row_idx < data_len;
                        let is_selected = self.selected == Some(row_idx);
                        row.set_selected(is_selected);

                        if is_data {
                            let path = &self.files[row_idx];
                            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("(invalid)");
                            let parent = path.parent().and_then(|p| p.to_str()).unwrap_or("");
                            // Ensure quick header meta is present when row is shown
                            if !self.meta.contains_key(path) {
                                if let Ok(reader) = hound::WavReader::open(path) {
                                    let spec = reader.spec();
                                    self.meta.insert(path.clone(), FileMeta { channels: spec.channels, sample_rate: spec.sample_rate, bits_per_sample: spec.bits_per_sample, duration_secs: None, rms_db: None, thumb: Vec::new() });
                                }
                            }
                            let meta = self.meta.get(path);

                            // col 0: File (clickable label with clipping)
                            row.col(|ui| {
                                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                    let resp = ui.add(
                                        egui::Label::new(RichText::new(name).size(text_height * 1.05))
                                            .sense(Sense::click())
                                            .truncate(true)
                                    ).on_hover_cursor(egui::CursorIcon::PointingHand);
                                    
                                    // シングルクリック: 行選択
                                    if resp.clicked() && !resp.double_clicked() {
                                        self.selected = Some(row_idx); 
                                        self.scroll_to_selected = true;
                                    }
                                    
                                    // ダブルクリック: エディタタブで開く
                                    if resp.double_clicked() {
                                        self.selected = Some(row_idx); 
                                        self.scroll_to_selected = true;
                                        to_open = Some(path.clone());
                                    }
                                    
                                    if resp.hovered() {
                                        resp.on_hover_text(name);
                                    }
                                });
                            });
                            // col 1: Folder (clickable label with clipping)
                            row.col(|ui| {
                                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                    let resp = ui.add(
                                        egui::Label::new(RichText::new(parent).monospace().size(text_height * 1.0))
                                            .sense(Sense::click())
                                            .truncate(true)
                                    ).on_hover_cursor(egui::CursorIcon::PointingHand);
                                    
                                    // シングルクリック: 行選択
                                    if resp.clicked() && !resp.double_clicked() {
                                        self.selected = Some(row_idx); 
                                        self.scroll_to_selected = true;
                                    }
                                    
                                    // ダブルクリック: システムのファイルブラウザでフォルダを開く（WAVファイルを選択状態で）
                                    if resp.double_clicked() {
                                        self.selected = Some(row_idx); 
                                        self.scroll_to_selected = true;
                                        // ファイルを選択状態でフォルダを開く
                                        let _ = open_folder_with_file_selected(path);
                                    }
                                    
                                    if resp.hovered() {
                                        resp.on_hover_text(parent);
                                    }
                                });
                            });
                            // col 2: Length (mm:ss) - clickable
                            row.col(|ui| {
                                let secs = meta.and_then(|m| m.duration_secs).unwrap_or(f32::NAN);
                                let text = if secs.is_finite() { format_duration(secs) } else { "...".into() };
                                let resp = ui.add(
                                    egui::Label::new(RichText::new(text).monospace())
                                        .sense(Sense::click())
                                ).on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked() {
                                    self.selected = Some(row_idx); 
                                    self.scroll_to_selected = true;
                                }
                            });
                            // col 3: Channels - clickable
                            row.col(|ui| {
                                let ch = meta.map(|m| m.channels).unwrap_or(0);
                                let resp = ui.add(
                                    egui::Label::new(RichText::new(format!("{}", ch)).monospace())
                                        .sense(Sense::click())
                                ).on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked() {
                                    self.selected = Some(row_idx); 
                                    self.scroll_to_selected = true;
                                }
                            });
                            // col 4: Sample rate - clickable
                            row.col(|ui| {
                                let sr = meta.map(|m| m.sample_rate).unwrap_or(0);
                                let resp = ui.add(
                                    egui::Label::new(RichText::new(format!("{}", sr)).monospace())
                                        .sense(Sense::click())
                                ).on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked() {
                                    self.selected = Some(row_idx); 
                                    self.scroll_to_selected = true;
                                }
                            });
                            // col 5: Bits per sample - clickable
                            row.col(|ui| {
                                let bits = meta.map(|m| m.bits_per_sample).unwrap_or(0);
                                let resp = ui.add(
                                    egui::Label::new(RichText::new(format!("{}", bits)).monospace())
                                        .sense(Sense::click())
                                ).on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked() {
                                    self.selected = Some(row_idx); 
                                    self.scroll_to_selected = true;
                                }
                            });
                            // col 6: Level (painted background + label) - clickable
                            row.col(|ui| {
                                let (rect2, resp2) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::click());
                                if let Some(m) = meta { if let Some(db) = m.rms_db { ui.painter().rect_filled(rect2, 4.0, db_to_color(db)); } }
                                let text = meta.and_then(|m| m.rms_db).map(|db| format!("{:.1}", db)).unwrap_or_else(|| "...".into());
                                let fid = TextStyle::Monospace.resolve(ui.style());
                                ui.painter().text(rect2.center(), egui::Align2::CENTER_CENTER, text, fid, Color32::WHITE);
                                if resp2.clicked() {
                                    self.selected = Some(row_idx); 
                                    self.scroll_to_selected = true;
                                }
                            });
                            // col 7: Wave thumbnail - clickable
                            row.col(|ui| {
                                let desired_w = ui.available_width().max(80.0);
                                let thumb_h = (desired_w * 0.22).clamp(text_height * 1.2, text_height * 4.0);
                                let (rect, painter) = ui.allocate_painter(egui::vec2(desired_w, thumb_h), Sense::click());
                                if row_idx == 0 { self.wave_row_h = thumb_h; }
                                if let Some(m) = meta { let w = rect.rect.width(); let h = rect.rect.height(); let n = m.thumb.len().max(1) as f32; for (idx, &(mn, mx)) in m.thumb.iter().enumerate() {
                                        let x = rect.rect.left() + (idx as f32 / n) * w; let y0 = rect.rect.center().y - mx * (h*0.45); let y1 = rect.rect.center().y - mn * (h*0.45);
                                        let a = (mn.abs().max(mx.abs())).clamp(0.0,1.0);
                                        let col = amp_to_color(a);
                                        painter.line_segment([egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))], egui::Stroke::new(1.0, col)); } }
                                if rect.clicked() {
                                    self.selected = Some(row_idx); 
                                    self.scroll_to_selected = true;
                                }
                            });
                            // col 8: Spacer (fills remainder so scrollbar stays at right edge)
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); });

                            // Row-level click handling (background/any non-interactive area)
                            let resp = row.response();
                            if resp.clicked() {
                                self.selected = Some(row_idx); 
                                self.scroll_to_selected = true;
                                let p_owned = path.clone();
                                // リスト表示時は常にループを無効にする
                                self.audio.set_loop_enabled(false);
                                match self.mode {
                                    RateMode::Speed => { let _ = prepare_for_speed(&p_owned, &self.audio, &mut Vec::new(), self.playback_rate); self.audio.set_rate(self.playback_rate); }
                                    _ => { self.audio.set_rate(1.0); self.spawn_heavy_processing(&p_owned); }
                                }
                            }
                            if is_selected && self.scroll_to_selected { resp.scroll_to_me(Some(Align::Center)); }
                        } else {
                            // filler row to extend frame
                            row.col(|_ui| {});
                            row.col(|_ui| {});
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // Length
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // Ch
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // SR
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // Bits
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // Level
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // Wave
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // Spacer
                        }
                    });
                });
                if sort_changed { self.apply_sort(); }
                if let Some(p) = to_open.as_ref() { self.open_or_activate_tab(p); }
                if self.files.is_empty() { ui.label("Select a folder to show list"); }
            }
        });
        // When switching tabs, ensure the active tab's audio is loaded and loop state applied.
        if let Some(p) = activate_path {
            // Reload audio for the activated tab only; do not touch stored waveform
            match self.mode {
                RateMode::Speed => { let _ = prepare_for_speed(&p, &self.audio, &mut Vec::new(), self.playback_rate); self.audio.set_rate(self.playback_rate); }
                _ => { self.audio.set_rate(1.0); self.spawn_heavy_processing(&p); }
            }
            if let Some(idx) = self.active_tab { if let Some(tab) = self.tabs.get(idx) { self.audio.set_loop_enabled(tab.loop_enabled); if let Some(buf) = self.audio.shared.samples.load().as_ref() { self.audio.set_loop_region(0, buf.len()); } } }
        }
        // Clear pending scroll flag after building the table
        self.scroll_to_selected = false;

        // Busy overlay (blocks input and shows loader)
        if self.processing.is_some() {
            use egui::{Id, LayerId, Order};
            let screen = ctx.screen_rect();
            // block input
            egui::Area::new("busy_block_input".into()).order(Order::Foreground).show(ctx, |ui| {
                let _ = ui.allocate_rect(screen, Sense::click_and_drag());
            });
            // darken background
            let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("busy_layer")));
            painter.rect_filled(screen, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 180));
            // centered box with spinner and text
            egui::Area::new("busy_center".into()).order(Order::Foreground).anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0)).show(ctx, |ui| {
                egui::Frame::window(ui.style()).show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add(egui::Spinner::new());
                        let msg = self.processing.as_ref().map(|p| p.msg.as_str()).unwrap_or("Processing...");
                        ui.label(RichText::new(msg).strong());
                    });
                });
            });
        }
        ctx.request_repaint_after(Duration::from_millis(16));
    }
}
