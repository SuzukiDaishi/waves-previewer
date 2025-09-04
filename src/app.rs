use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use egui::{Align, Color32, FontData, FontDefinitions, FontFamily, FontId, Key, RichText, Sense, TextStyle, Visuals};
use egui_extras::TableBuilder;
use crate::audio::AudioEngine;
use crate::wave::{build_minmax, decode_wav_mono, prepare_for_speed, process_pitchshift_offline, process_timestretch_offline};
use walkdir::WalkDir;

pub struct EditorTab {
    pub path: PathBuf,
    pub display_name: String,
    pub waveform_minmax: Vec<(f32, f32)>,
    pub loop_enabled: bool,
}

pub struct FileMeta {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub duration_secs: Option<f32>,
    pub rms_db: Option<f32>,
    pub thumb: Vec<(f32, f32)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey { File, Folder, Length, Channels, SampleRate, Bits, Level }
#[derive(Clone, Copy, PartialEq, Eq)]
enum SortDir { Asc, Desc, None }

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum RateMode { Speed, PitchShift, TimeStretch }

struct ProcessingState {
    msg: String,
    path: PathBuf,
    rx: std::sync::mpsc::Receiver<ProcessingResult>,
}

struct ProcessingResult {
    path: PathBuf,
    samples: Vec<f32>,
    waveform: Vec<(f32, f32)>,
}

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

    pub fn rescan(&mut self) {
        self.files.clear();
        self.all_files.clear();
        if let Some(root) = &self.root {
            for entry in WalkDir::new(root).follow_links(false) {
                if let Ok(e) = entry {
                    if e.file_type().is_file() {
                        if let Some(ext) = e.path().extension().and_then(|s| s.to_str()) {
                            if ext.eq_ignore_ascii_case("wav") { self.all_files.push(e.into_path()); }
                        }
                    }
                }
            }
            self.all_files.sort();
        }
        // apply search filter and initialize files/original order
        self.apply_filter_from_search();
        self.meta.clear();
        if !self.all_files.is_empty() { self.meta_rx = Some(spawn_meta_worker(self.all_files.clone())); }
        // keep selection mapped to same path after rescan (best-effort)
        self.apply_sort();
    }

    fn open_or_activate_tab(&mut self, path: &Path) {
        // タブを開く/アクティブ化する時に音声を停止
        self.audio.stop();
        
        if let Some(idx) = self.tabs.iter().position(|t| t.path.as_path() == path) {
            self.active_tab = Some(idx); return;
        }
        match self.mode {
            RateMode::Speed => {
                let mut wf = Vec::new();
                if let Err(e) = prepare_for_speed(path, &self.audio, &mut wf, self.playback_rate) { eprintln!("load error: {e:?}") }
                self.audio.set_rate(self.playback_rate);
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("(invalid)").to_string();
                self.tabs.push(EditorTab { path: path.to_path_buf(), display_name: name, waveform_minmax: wf, loop_enabled: false });
                self.active_tab = Some(self.tabs.len() - 1);
            }
            _ => {
                // Heavy: create tab immediately with empty waveform, then spawn processing
                self.audio.set_rate(1.0);
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("(invalid)").to_string();
                self.tabs.push(EditorTab { path: path.to_path_buf(), display_name: name, waveform_minmax: Vec::new(), loop_enabled: false });
                self.active_tab = Some(self.tabs.len() - 1);
                self.spawn_heavy_processing(path);
            }
        }
    }

    // Merge helper: add a folder recursively (WAV only)
    fn add_folder_merge(&mut self, dir: &Path) -> usize {
        let mut added = 0usize;
        let mut existing: HashSet<PathBuf> = self.all_files.iter().cloned().collect();
        for entry in WalkDir::new(dir).follow_links(false) {
            if let Ok(e) = entry {
                if e.file_type().is_file() {
                    let p = e.into_path();
                    if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                        if ext.eq_ignore_ascii_case("wav") {
                            if existing.insert(p.clone()) { self.all_files.push(p); added += 1; }
                        }
                    }
                }
            }
        }
        self.all_files.sort();
        added
    }

    // Merge helper: add explicit files (WAV only)
    fn add_files_merge(&mut self, paths: &[PathBuf]) -> usize {
        let mut added = 0usize;
        let mut existing: HashSet<PathBuf> = self.all_files.iter().cloned().collect();
        for p in paths {
            if p.is_file() {
                if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                    if ext.eq_ignore_ascii_case("wav") {
                        if existing.insert(p.clone()) { self.all_files.push(p.clone()); added += 1; }
                    }
                }
            } else if p.is_dir() {
                added += self.add_folder_merge(p.as_path());
            }
        }
        self.all_files.sort();
        added
    }

    fn after_add_refresh(&mut self) {
        self.apply_filter_from_search();
        self.apply_sort();
        if !self.all_files.is_empty() { self.meta_rx = Some(spawn_meta_worker(self.all_files.clone())); }
    }

    // Replace current list with explicit files (WAV only). Root is cleared.
    fn replace_with_files(&mut self, paths: &[PathBuf]) {
        self.root = None;
        self.files.clear();
        self.all_files.clear();
        let mut set: HashSet<PathBuf> = HashSet::new();
        for p in paths {
            if p.is_file() {
                if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                    if ext.eq_ignore_ascii_case("wav") {
                        if set.insert(p.clone()) { self.all_files.push(p.clone()); }
                    }
                }
            }
        }
        self.all_files.sort();
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
                // make waveform taller as width grows, respecting remaining height
                let wave_h = (avail.x * 0.35).clamp(180.0, avail.y);
                let (rect, painter) = ui.allocate_painter(egui::vec2(avail.x, wave_h), Sense::hover());
                let w = rect.rect.width().max(1.0); let h = rect.rect.height().max(1.0);
                painter.rect_filled(rect.rect, 0.0, Color32::from_rgb(16,16,18));
                for g in 1..5 { let y = rect.rect.top() + h*(g as f32)/5.0; painter.line_segment([egui::pos2(rect.rect.left(), y), egui::pos2(rect.rect.right(), y)], egui::Stroke::new(1.0, Color32::from_rgb(45,45,50))); }
                if !tab.waveform_minmax.is_empty() {
                    let n = tab.waveform_minmax.len() as f32;
                    for (idx, &(mn, mx)) in tab.waveform_minmax.iter().enumerate() {
                        let x = rect.rect.left() + (idx as f32 / n) * w;
                        let y0 = rect.rect.center().y - mx * (h*0.48);
                        let y1 = rect.rect.center().y - mn * (h*0.48);
                        let amp = (mn.abs().max(mx.abs())).clamp(0.0, 1.0);
                        let col = amp_to_color(amp);
                        painter.line_segment([egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))], egui::Stroke::new(1.0, col));
                    }
                }
                if let Some(buf) = self.audio.shared.samples.load().as_ref() {
                    let len = buf.len().max(1);
                    let pos = self.audio.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed).min(len);
                    let x = rect.rect.left() + (pos as f32 / len as f32) * w;
                    painter.line_segment([egui::pos2(x, rect.rect.top()), egui::pos2(x, rect.rect.bottom())], egui::Stroke::new(2.0, Color32::from_rgb(70,140,255)));
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

fn spawn_meta_worker(paths: Vec<PathBuf>) -> std::sync::mpsc::Receiver<(PathBuf, FileMeta)> {
    use std::sync::mpsc; let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for p in paths {
            // Stage 1: quick header-only metadata
            if let Ok(reader) = hound::WavReader::open(&p) {
                let spec = reader.spec();
                let _ = tx.send((p.clone(), FileMeta{
                    channels: spec.channels,
                    sample_rate: spec.sample_rate,
                    bits_per_sample: spec.bits_per_sample,
                    duration_secs: None,
                    rms_db: None,
                    thumb: Vec::new(),
                }));
            }
            // Stage 2: decode for RMS and thumbnail
            if let Ok((mono, _sr)) = decode_wav_mono(&p) {
                let mut sum_sq = 0.0f64;
                for &v in &mono { sum_sq += (v as f64)*(v as f64); }
                let n = mono.len().max(1) as f64;
                let rms = (sum_sq/n).sqrt() as f32;
                let rms_db = if rms>0.0 { 20.0*rms.log10() } else { -120.0 };
                let mut thumb = Vec::new();
                build_minmax(&mut thumb, &mono, 128);
                // attempt to reuse spec (optional)
                let (ch, sr, bits) = if let Ok(reader2) = hound::WavReader::open(&p) { let s = reader2.spec(); (s.channels, s.sample_rate, s.bits_per_sample) } else { (0,0,0) };
                let length_secs = if sr > 0 { mono.len() as f32 / sr as f32 } else { f32::NAN };
                let _ = tx.send((p, FileMeta{ channels: ch, sample_rate: sr, bits_per_sample: bits, duration_secs: Some(length_secs), rms_db: Some(rms_db), thumb }));
            }
        }
    });
    rx
}

fn db_to_amp(db: f32) -> f32 { if db <= -80.0 { 0.0 } else { (10.0f32).powf(db/20.0) } }

fn db_to_color(db: f32) -> Color32 {
    // Expanded palette for clearer perception across ranges.
    // Control points: (dBFS, Color)
    let pts: &[(f32, Color32)] = &[
        (-80.0, Color32::from_rgb(10, 10, 12)),   // near silence
        (-60.0, Color32::from_rgb(20, 50, 110)),  // deep blue
        (-40.0, Color32::from_rgb(40, 100, 180)), // blue
        (-25.0, Color32::from_rgb(80, 200, 255)), // cyan/teal
        (-12.0, Color32::from_rgb(220, 220, 60)), // yellow
        (0.0,   Color32::from_rgb(255, 150, 60)), // orange
        (6.0,   Color32::from_rgb(255, 70, 70)),  // red (near 0 dBFS+)
    ];
    let x = db.clamp(pts.first().unwrap().0, pts.last().unwrap().0);
    // find segment
    for w in pts.windows(2) {
        let (x0, c0) = w[0];
        let (x1, c1) = w[1];
        if x >= x0 && x <= x1 {
            let t = if (x1 - x0).abs() < f32::EPSILON { 0.0 } else { (x - x0) / (x1 - x0) };
            return lerp_color(c0, c1, t);
        }
    }
    pts.last().unwrap().1
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 { let t = t.clamp(0.0,1.0); let r = (a.r() as f32 + (b.r() as f32 - a.r() as f32)*t) as u8; let g = (a.g() as f32 + (b.g() as f32 - a.g() as f32)*t) as u8; let bl = (a.b() as f32 + (b.b() as f32 - a.b() as f32)*t) as u8; Color32::from_rgb(r,g,bl) }

fn amp_to_color(a: f32) -> Color32 {
    let t = a.clamp(0.0, 1.0).powf(0.6); // emphasize loud parts
    lerp_color(Color32::from_rgb(80,200,255), Color32::from_rgb(255,70,70), t)
}

fn open_in_file_explorer(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        Command::new("explorer").arg(path).spawn()?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        Command::new("open").arg(path).spawn()?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use std::process::Command;
        Command::new("xdg-open").arg(path).spawn()?;
        Ok(())
    }
}

fn open_folder_with_file_selected(file_path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        // Windows: /select パラメータでファイルを選択状態でフォルダを開く
        Command::new("explorer").arg("/select,").arg(file_path).spawn()?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        // macOS: -R フラグでFinderでファイルを選択状態で開く
        Command::new("open").arg("-R").arg(file_path).spawn()?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use std::process::Command;
        // Linux: ファイルマネージャーでフォルダを開く (ファイル選択は一般的にサポートされていない)
        if let Some(parent) = file_path.parent() {
            Command::new("xdg-open").arg(parent).spawn()?;
        }
        Ok(())
    }
}

fn sortable_header(
    ui: &mut egui::Ui,
    label: &str,
    sort_key: &mut SortKey,
    sort_dir: &mut SortDir,
    key: SortKey,
    default_asc: bool,
) -> bool {
    let is_active = *sort_key == key && *sort_dir != SortDir::None;
    let arrow = if is_active { match *sort_dir { SortDir::Asc => " ▲", SortDir::Desc => " ▼", SortDir::None => "" } } else { "" };
    let btn = egui::Button::new(RichText::new(format!("{}{}", label, arrow)).strong());
    let clicked = ui.add(btn).clicked();
    if clicked {
        if *sort_key != key {
            *sort_key = key;
            *sort_dir = if default_asc { SortDir::Asc } else { SortDir::Desc };
        } else {
            *sort_dir = match *sort_dir { SortDir::Asc => SortDir::Desc, SortDir::Desc => SortDir::None, SortDir::None => if default_asc { SortDir::Asc } else { SortDir::Desc } };
        }
        return true;
    }
    false
}

impl WavesPreviewer {
    fn apply_filter_from_search(&mut self) {
        // Preserve selection path if possible
        let selected_path: Option<PathBuf> = self.selected.and_then(|i| self.files.get(i).cloned());
        if self.search_query.trim().is_empty() {
            self.files = self.all_files.clone();
        } else {
            let q = self.search_query.to_lowercase();
            self.files = self.all_files.iter().filter(|p| {
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
                let parent = p.parent().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
                name.contains(&q) || parent.contains(&q)
            }).cloned().collect();
        }
        self.original_files = self.files.clone();
        // restore selected index
        self.selected = selected_path.and_then(|p| self.files.iter().position(|x| *x == p));
    }
    fn apply_sort(&mut self) {
        if self.files.is_empty() { return; }
        let selected_path: Option<PathBuf> = self.selected.and_then(|i| self.files.get(i).cloned());
        let key = self.sort_key;
        let dir = self.sort_dir;
        if dir == SortDir::None {
            self.files = self.original_files.clone();
        } else {
            self.files.sort_by(|a, b| {
                use std::cmp::Ordering;
                let ord = match key {
                    SortKey::File => {
                        let sa = a.file_name().and_then(|s| s.to_str()).unwrap_or("");
                        let sb = b.file_name().and_then(|s| s.to_str()).unwrap_or("");
                        sa.cmp(sb)
                    }
                    SortKey::Folder => {
                        let sa = a.parent().and_then(|p| p.to_str()).unwrap_or("");
                        let sb = b.parent().and_then(|p| p.to_str()).unwrap_or("");
                        sa.cmp(sb)
                    }
                    SortKey::Length => num_order(self.meta.get(a).and_then(|m| m.duration_secs).unwrap_or(0.0),
                                                 self.meta.get(b).and_then(|m| m.duration_secs).unwrap_or(0.0)),
                    SortKey::Channels => num_order(self.meta.get(a).map(|m| m.channels as f32).unwrap_or(0.0),
                                                   self.meta.get(b).map(|m| m.channels as f32).unwrap_or(0.0)),
                    SortKey::SampleRate => num_order(self.meta.get(a).map(|m| m.sample_rate as f32).unwrap_or(0.0),
                                                     self.meta.get(b).map(|m| m.sample_rate as f32).unwrap_or(0.0)),
                    SortKey::Bits => num_order(self.meta.get(a).map(|m| m.bits_per_sample as f32).unwrap_or(0.0),
                                               self.meta.get(b).map(|m| m.bits_per_sample as f32).unwrap_or(0.0)),
                    SortKey::Level => num_order(self.meta.get(a).and_then(|m| m.rms_db).unwrap_or(f32::NEG_INFINITY),
                                                self.meta.get(b).and_then(|m| m.rms_db).unwrap_or(f32::NEG_INFINITY)),
                };
                match dir { SortDir::Asc => ord, SortDir::Desc => ord.reverse(), SortDir::None => Ordering::Equal }
            });
        }

        // restore selection to the same path if possible
        self.selected = selected_path.and_then(|p| self.files.iter().position(|x| *x == p));
    }
}

fn num_order(a: f32, b: f32) -> std::cmp::Ordering {
    a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
}

fn format_duration(secs: f32) -> String {
    let s = if secs.is_finite() && secs >= 0.0 { secs } else { 0.0 };
    let total = s.round() as u64;
    let m = total / 60;
    let s = total % 60;
    format!("{}:{:02}", m, s)
}

impl WavesPreviewer {
    fn current_path_for_rebuild(&self) -> Option<PathBuf> {
        if let Some(i) = self.active_tab { return self.tabs.get(i).map(|t| t.path.clone()); }
        if let Some(i) = self.selected { return self.files.get(i).cloned(); }
        None
    }

    fn rebuild_current_buffer_with_mode(&mut self) {
        if let Some(p) = self.current_path_for_rebuild() {
            match self.mode {
                RateMode::Speed => { let _ = prepare_for_speed(&p, &self.audio, &mut Vec::new(), self.playback_rate); self.audio.set_rate(self.playback_rate); }
                _ => { self.audio.set_rate(1.0); self.spawn_heavy_processing(&p); }
            }
        }
    }

    fn spawn_heavy_processing(&mut self, path: &Path) {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<ProcessingResult>();
        let path_buf = path.to_path_buf();
        let mode = self.mode;
        let rate = self.playback_rate;
        let sem = self.pitch_semitones;
        let out_sr = self.audio.shared.out_sample_rate;
        let path_for_thread = path_buf.clone();
        std::thread::spawn(move || {
            // heavy decode and process
            if let Ok((mono, in_sr)) = decode_wav_mono(&path_for_thread) {
                let samples = match mode {
                    RateMode::PitchShift => process_pitchshift_offline(&mono, in_sr, out_sr, sem),
                    RateMode::TimeStretch => process_timestretch_offline(&mono, in_sr, out_sr, rate),
                    RateMode::Speed => mono, // not used
                };
                let mut waveform = Vec::new();
                build_minmax(&mut waveform, &samples, 2048);
                let _ = tx.send(ProcessingResult { path: path_for_thread.clone(), samples, waveform });
            }
        });
        self.processing = Some(ProcessingState { msg: match mode { RateMode::PitchShift => "Pitch-shifting...".to_string(), RateMode::TimeStretch => "Time-stretching...".to_string(), RateMode::Speed => "Processing...".to_string() }, path: path_buf, rx });
    }
}
