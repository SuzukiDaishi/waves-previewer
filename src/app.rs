use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use egui::{Align, Color32, FontData, FontDefinitions, FontFamily, FontId, Key, RichText, Sense, TextStyle, Visuals};
use egui_extras::TableBuilder;
use crate::audio::AudioEngine;
use crate::wave::{build_minmax, prepare_for_speed};
// use walkdir::WalkDir; // unused here (used in logic.rs)

mod types;
mod helpers;
mod meta;
mod logic;
mod ui;
use self::{types::*, helpers::*};

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
    // multi-selection (list view)
    pub selected_multi: std::collections::BTreeSet<usize>,
    pub select_anchor: Option<usize>,
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
    // per-file pending gain edits (dB)
    pending_gains: HashMap<PathBuf, f32>,
    // background export state (gains)
    export_state: Option<ExportState>,
    // currently loaded/playing file path (for effective volume calc)
    playing_path: Option<PathBuf>,
    // export/save settings (simple, in-memory)
    export_cfg: ExportConfig,
    show_export_settings: bool,
    show_first_save_prompt: bool,
        saving_sources: Vec<PathBuf>,
        saving_mode: Option<SaveMode>,

        // LUFS with Gain recompute support
        lufs_override: HashMap<PathBuf, f32>,
        lufs_recalc_deadline: HashMap<PathBuf, std::time::Instant>,
        lufs_rx2: Option<std::sync::mpsc::Receiver<(PathBuf, f32)>>,
        lufs_worker_busy: bool,
        // leaving dirty editor confirmation
        leave_intent: Option<LeaveIntent>,
        show_leave_prompt: bool,
        pending_activate_path: Option<PathBuf>,
    }

impl WavesPreviewer {
    fn editor_selected_range(tab: &EditorTab) -> Option<(usize,usize)> {
        if let Some(r) = tab.selection { if r.1 > r.0 { return Some(r); } }
        if let Some(r) = tab.ab_loop { if r.1 != r.0 { let (a,b) = if r.0<=r.1 {(r.0,r.1)} else {(r.1,r.0)}; return Some((a,b)); } }
        None
    }
    fn editor_mixdown_mono(tab: &EditorTab) -> Vec<f32> {
        let n = tab.samples_len;
        if n == 0 { return Vec::new(); }
        if tab.ch_samples.is_empty() { return vec![0.0; n]; }
        let chn = tab.ch_samples.len() as f32;
        let mut out = vec![0.0f32; n];
        for ch in &tab.ch_samples { for i in 0..n { if let Some(&v)=ch.get(i) { out[i]+=v; } } }
        for v in &mut out { *v /= chn; }
        out
    }
        fn editor_apply_trim_range(&mut self, tab_idx: usize, range: (usize,usize)) {
        let (mono, ab, len) = {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                let (s,e) = range; if e<=s || e>tab.samples_len { return; }
                for ch in tab.ch_samples.iter_mut() { let mut seg = ch[s..e].to_vec(); std::mem::swap(ch, &mut seg); ch.truncate(e-s); }
                tab.samples_len = e - s;
                tab.view_offset = 0; tab.selection = Some((0, tab.samples_len));
                tab.ab_loop = None; tab.dirty = true;
                (Self::editor_mixdown_mono(tab), tab.ab_loop, tab.samples_len)
            } else { return; }
        };
        self.audio.set_samples(std::sync::Arc::new(mono));
        self.audio.stop();
        if let Some((a,b)) = ab { let (s,e) = if a<=b {(a,b)} else {(b,a)}; self.audio.set_loop_region(s,e); }
        else { self.audio.set_loop_region(0, len); }
    }
    fn editor_apply_fade_range(&mut self, tab_idx: usize, range: (usize,usize), in_ms: f32, out_ms: f32) {
        let (mono, ab, len) = {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                let (s, e) = range; if e<=s || e>tab.samples_len { return; }
                let sr = self.audio.shared.out_sample_rate.max(1) as f32;
                let in_samp = ((in_ms/1000.0)*sr).round() as usize;
                let out_samp = ((out_ms/1000.0)*sr).round() as usize;
                let in_len = in_samp.min(e.saturating_sub(s));
                let out_len = out_samp.min(e.saturating_sub(s));
                for ch in tab.ch_samples.iter_mut() { for i in 0..in_len { let t = (i as f32)/(in_len.max(1) as f32); ch[s+i] *= t; } }
                for ch in tab.ch_samples.iter_mut() { for i in 0..out_len { let t = 1.0 - (i as f32)/(out_len.max(1) as f32); ch[e-1-i] *= t; } }
                tab.dirty = true;
                (Self::editor_mixdown_mono(tab), tab.ab_loop, tab.samples_len)
            } else { return; }
        };
        self.audio.set_samples(std::sync::Arc::new(mono));
        self.audio.stop();
        if let Some((a,b)) = ab { let (s,e) = if a<=b {(a,b)} else {(b,a)}; self.audio.set_loop_region(s,e); }
        else { self.audio.set_loop_region(0, len); }
    }
    fn apply_loop_mode_for_tab(&self, tab: &EditorTab) {
        match tab.loop_mode {
            LoopMode::Off => { self.audio.set_loop_enabled(false); }
            LoopMode::OnWhole => {
                self.audio.set_loop_enabled(true);
                if let Some(buf) = self.audio.shared.samples.load().as_ref() { self.audio.set_loop_region(0, buf.len()); }
            }
            LoopMode::Marker => {
                if let Some((a,b)) = tab.ab_loop { if a!=b { let (s,e) = if a<=b {(a,b)} else {(b,a)}; self.audio.set_loop_enabled(true); self.audio.set_loop_region(s,e); return; } }
                self.audio.set_loop_enabled(false);
            }
        }
    }
    fn set_marker_sample(tab: &mut EditorTab, idx: usize) {
        match tab.ab_loop {
            None => tab.ab_loop = Some((idx, idx)),
            Some((a,b)) => {
                if a==b { tab.ab_loop = Some((a.min(idx), a.max(idx))); }
                else { let da = a.abs_diff(idx); let db = b.abs_diff(idx); if da <= db { tab.ab_loop = Some((idx, b)); } else { tab.ab_loop = Some((a, idx)); } }
            }
        }
    }fn current_active_path(&self) -> Option<&PathBuf> {
        if let Some(i) = self.active_tab { return self.tabs.get(i).map(|t| &t.path); }
        if let Some(i) = self.selected { return self.files.get(i); }
        None
    }
    pub(super) fn apply_effective_volume(&self) {
        // Global output volume (0..1)
        let base = db_to_amp(self.volume_db);
        self.audio.set_volume(base);
        // Per-file gain (can be >1)
        let path_opt = self.playing_path.as_ref().or_else(|| self.current_active_path());
        let gain_db = if let Some(p) = path_opt { *self.pending_gains.get(p).unwrap_or(&0.0) } else { 0.0 };
        let fg = db_to_amp(gain_db);
        self.audio.set_file_gain(fg);
    }
    fn spawn_export_gains(&mut self, _overwrite: bool) {
        use std::sync::mpsc;
        let mut targets: Vec<(PathBuf, f32)> = Vec::new();
        for p in &self.all_files { if let Some(db) = self.pending_gains.get(p) { if db.abs() > 0.0001 { targets.push((p.clone(), *db)); } } }
        if targets.is_empty() { return; }
        let (tx, rx) = mpsc::channel::<ExportResult>();
        std::thread::spawn(move || {
            let mut ok = 0usize; let mut failed = 0usize; let mut success_paths = Vec::new(); let mut failed_paths = Vec::new();
            for (src, db) in targets {
                let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
                let dst = src.with_file_name(format!("{} (gain{:+.1}dB).wav", stem, db));
                match crate::wave::export_gain_wav(&src, &dst, db) { Ok(_) => { ok += 1; success_paths.push(dst); }, Err(e) => { eprintln!("export failed {}: {e:?}", src.display()); failed += 1; failed_paths.push(src.clone()); } }
            }
            let _ = tx.send(ExportResult{ ok, failed, success_paths, failed_paths });
        });
        self.export_state = Some(ExportState{ msg: "Exporting gains".into(), rx });
    }


    fn trigger_save_selected(&mut self) {
        if self.export_cfg.first_prompt { self.show_first_save_prompt = true; return; }
        let mut set = self.selected_multi.clone();
        if set.is_empty() { if let Some(i) = self.selected { set.insert(i); } }
        self.spawn_save_selected(set);
    }

    fn spawn_save_selected(&mut self, indices: std::collections::BTreeSet<usize>) {
        use std::sync::mpsc;
        if indices.is_empty() { return; }
        let mut items: Vec<(PathBuf, f32)> = Vec::new();
        for i in indices { if let Some(p) = self.files.get(i) { if let Some(db) = self.pending_gains.get(p) { if db.abs()>0.0001 { items.push((p.clone(), *db)); } } } }
        if items.is_empty() { return; }
        let cfg = self.export_cfg.clone();
        // remember sources for post-save cleanup + reload
        self.saving_sources = items.iter().map(|(p,_)| p.clone()).collect();
        self.saving_mode = Some(cfg.save_mode);
        let (tx, rx) = mpsc::channel::<ExportResult>();
        std::thread::spawn(move || {
            let mut ok=0usize; let mut failed=0usize; let mut success_paths=Vec::new(); let mut failed_paths=Vec::new();
            for (src, db) in items {
                match cfg.save_mode {
                    SaveMode::Overwrite => {
                        match crate::wave::overwrite_gain_wav(&src, db, cfg.backup_bak) {
                            Ok(()) => { ok+=1; success_paths.push(src.clone()); },
                            Err(_)  => { failed+=1; failed_paths.push(src.clone()); }
                        }
                    }
                    SaveMode::NewFile => {
                        let parent = cfg.dest_folder.clone().unwrap_or_else(|| src.parent().unwrap_or_else(|| std::path::Path::new(".")).to_path_buf());
                        let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
                        let mut name = cfg.name_template.clone();
                        name = name.replace("{name}", stem);
                        name = name.replace("{gain:+.1}", &format!("{:+.1}", db));
                        name = name.replace("{gain:+0.0}", &format!("{:+.1}", db));
                        name = name.replace("{gain}", &format!("{:+.1}", db));
                        let name = crate::app::helpers::sanitize_filename_component(&name);
                        let mut dst = parent.join(name);
                        match dst.extension().and_then(|e| e.to_str()) { Some(ext) if ext.eq_ignore_ascii_case("wav") => {}, _ => { dst.set_extension("wav"); } }
                        if dst.exists() {
                            match cfg.conflict {
                                ConflictPolicy::Overwrite => {}
                                ConflictPolicy::Skip => { failed+=1; failed_paths.push(src.clone()); continue; }
                                ConflictPolicy::Rename => {
                                    let orig = dst.clone();
                                    let mut idx=1u32; loop {
                                        let stem2 = orig.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
                                        let n = crate::app::helpers::sanitize_filename_component(&format!("{}_{:02}", stem2, idx));
                                        dst = orig.with_file_name(n);
                                        match dst.extension().and_then(|e| e.to_str()) { Some(ext) if ext.eq_ignore_ascii_case("wav") => {}, _ => { dst.set_extension("wav"); } }
                                        if !dst.exists() { break; }
                                        idx+=1; if idx>999 { break; }
                                    }
                                }
                            }
                        }
                        match crate::wave::export_gain_wav(&src, &dst, db) {
                            Ok(()) => { ok+=1; success_paths.push(dst.clone()); },
                            Err(_)  => { failed+=1; failed_paths.push(src.clone()); }
                        }
                    }
                }
            }
            let _=tx.send(ExportResult{ ok, failed, success_paths, failed_paths });
        });
        self.export_state = Some(ExportState{ msg: "Saving...".into(), rx });
    }

    // moved to logic.rs: update_selection_on_click

    // --- Gain helpers ---
    fn clamp_gain_db(val: f32) -> f32 {
        let mut g = val.clamp(-24.0, 24.0);
        if g.abs() < 0.001 { g = 0.0; }
        g
    }

    fn adjust_gain_for_indices(&mut self, indices: &std::collections::BTreeSet<usize>, delta_db: f32) {
        if indices.is_empty() { return; }
        let mut affect_playing = false;
        for &i in indices {
            if let Some(p) = self.files.get(i).cloned() {
                let cur = *self.pending_gains.get(&p).unwrap_or(&0.0);
                let new = Self::clamp_gain_db(cur + delta_db);
                if new == 0.0 { self.pending_gains.remove(&p); } else { self.pending_gains.insert(p.clone(), new); }
                if self.playing_path.as_ref() == Some(&p) { affect_playing = true; }
                // schedule LUFS recompute for each affected path
                self.schedule_lufs_for_path(p.clone());
            }
        }
        if affect_playing { self.apply_effective_volume(); }
    }

    fn schedule_lufs_for_path(&mut self, path: PathBuf) {
        use std::time::{Duration, Instant};
        let dl = Instant::now() + Duration::from_millis(400);
        self.lufs_recalc_deadline.insert(path, dl);
    }
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
        // 初期状態（リスト表示�E�ではループを無効にする
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
            selected_multi: std::collections::BTreeSet::new(),
            select_anchor: None,
            sort_key: SortKey::File,
            sort_dir: SortDir::None,
            scroll_to_selected: false,
            original_files: Vec::new(),
            search_query: String::new(),
            mode: RateMode::Speed,
            processing: None,
            pending_gains: HashMap::new(),
            export_state: None,
            playing_path: None,

            export_cfg: ExportConfig { first_prompt: true, save_mode: SaveMode::NewFile, dest_folder: None, name_template: "{name} (gain{gain:+.1}dB)".into(), conflict: ConflictPolicy::Rename, backup_bak: true },
            show_export_settings: false,
            show_first_save_prompt: false,
            saving_sources: Vec::new(),
            saving_mode: None,

            lufs_override: HashMap::new(),
            lufs_recalc_deadline: HashMap::new(),
            lufs_rx2: None,
            lufs_worker_busy: false,
            leave_intent: None,
            show_leave_prompt: false,
            pending_activate_path: None,

        })
    }

}

impl eframe::App for WavesPreviewer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Ensure effective volume (global vol x per-file gain) is always applied
        self.apply_effective_volume();
        // Drain metadata updates
        if let Some(rx) = &self.meta_rx {
            let mut resort = false;
            while let Ok((p, m)) = rx.try_recv() { self.meta.insert(p, m); resort = true; }
            if resort { self.apply_sort(); ctx.request_repaint(); }
        }

        // Drain export results
        if let Some(state) = &self.export_state {
            if let Ok(res) = state.rx.try_recv() {
                eprintln!("save/export done: ok={}, failed={}", res.ok, res.failed);
                if state.msg.starts_with("Saving") {
                    for p in &self.saving_sources { self.pending_gains.remove(p); self.lufs_override.remove(p); }
                    match self.saving_mode.unwrap_or(self.export_cfg.save_mode) {
                        SaveMode::Overwrite => {
                            if !self.saving_sources.is_empty() { self.meta_rx = Some(meta::spawn_meta_worker(self.saving_sources.clone())); }
                            if let Some(path) = self.saving_sources.get(0).cloned() {
                                if let Some(idx) = self.files.iter().position(|x| *x == path) { self.select_and_load(idx); }
                            }
                        }
                        SaveMode::NewFile => {
                            let mut added_any=false; let mut first_added=None;
                            for p in &res.success_paths { if self.add_files_merge(&[p.clone()])>0 { if first_added.is_none(){ first_added=Some(p.clone()); } added_any=true; } }
                            if added_any { self.after_add_refresh(); }
                            if let Some(p) = first_added { if let Some(idx) = self.files.iter().position(|x| *x == p) { self.select_and_load(idx); } }
                        }
                    }
                    self.saving_sources.clear(); self.saving_mode=None;
                }
                self.export_state = None;
                ctx.request_repaint();
            }
        }

        // Drain LUFS (with gain) recompute results
        let mut got_any = false;
        if let Some(rx) = &self.lufs_rx2 {
            while let Ok((p, v)) = rx.try_recv() { self.lufs_override.insert(p, v); got_any = true; }
        }
        if got_any { self.lufs_worker_busy = false; }

        // Pump LUFS recompute worker (debounced)
        if !self.lufs_worker_busy {
            let now = std::time::Instant::now();
            if let Some(path) = self.lufs_recalc_deadline.iter().find(|(_, dl)| **dl <= now).map(|(p, _)| p.clone()) {
                self.lufs_recalc_deadline.remove(&path);
                let g_db = *self.pending_gains.get(&path).unwrap_or(&0.0);
                if g_db.abs() < 0.0001 { self.lufs_override.remove(&path); }
                else {
                    use std::sync::mpsc; let (tx, rx) = mpsc::channel();
                    self.lufs_rx2 = Some(rx);
                    self.lufs_worker_busy = true;
                    std::thread::spawn(move || {
                        let res = (|| -> anyhow::Result<f32> {
                            let (mut chans, sr) = crate::wave::decode_wav_multi(&path)?;
                            let gain = 10.0f32.powf(g_db/20.0);
                            for ch in chans.iter_mut() { for v in ch.iter_mut() { *v *= gain; } }
                            crate::wave::lufs_integrated_from_multi(&chans, sr)
                        })();
                        let val = match res { Ok(v) => v, Err(_) => f32::NEG_INFINITY };
                        let _=tx.send((path, val));
                        });
                }
            }
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
                // update current playing path (for effective volume using pending gains)
                self.playing_path = Some(res.path.clone());
                // full-buffer loop region if needed
                if let Some(buf) = self.audio.shared.samples.load().as_ref() { self.audio.set_loop_region(0, buf.len()); }
                self.processing = None;
                ctx.request_repaint();
            }
        }

        // Shortcuts
        if ctx.input(|i| i.key_pressed(Key::Space)) { self.audio.toggle_play(); }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(Key::S)) { self.trigger_save_selected(); }
        
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(Key::W)) {
            if let Some(active_idx) = self.active_tab {
                self.audio.stop();
                self.tabs.remove(active_idx);
                // 閉じたタブ�E後にタブがあれば次のタブ、なければ前�EタブをアクチE��ブに
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
        
        // Top controls (always visible)
        self.ui_top_bar(ctx);
        let mut activate_path: Option<PathBuf> = None;
        egui::CentralPanel::default().show(ctx, |ui| {            // Tabs
            ui.horizontal_wrapped(|ui| {
                let is_list = self.active_tab.is_none();
                let list_label = if is_list { RichText::new("[List]").strong() } else { RichText::new("List") };
                if ui.selectable_label(is_list, list_label).clicked() {
                    self.active_tab = None;
                    self.audio.stop();
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
                            self.audio.stop();
                        }
                        if ui.button("x").on_hover_text("Close").clicked() {
                            to_close = Some(i);
                            self.audio.stop();
                        }
                    });
                }
                if let Some(i) = to_close {
                    self.tabs.remove(i);
                    match self.active_tab {
                        Some(ai) if ai == i => self.active_tab = None,
                        Some(ai) if ai > i => self.active_tab = Some(ai - 1),
                        _ => {}
                    }
                }
            });
            ui.separator();
        let mut apply_pending_loop = false;
        if let Some(tab_idx) = self.active_tab {
    let tab = &mut self.tabs[tab_idx];
    ui.horizontal(|ui| {
        let dirty_mark = if tab.dirty { " •" } else { "" };
        ui.label(RichText::new(format!("{}{}", tab.path.display(), dirty_mark)).monospace());
        ui.separator();
        ui.label("Loop:");
        for (m,label) in [ (LoopMode::Off, "Off"), (LoopMode::OnWhole, "On"), (LoopMode::Marker, "Marker") ] {
            if ui.selectable_label(tab.loop_mode == m, label).clicked() {
                tab.loop_mode = m;
                apply_pending_loop = true;
            }
        }
        ui.separator();
        // A/B status and quick set
        let sr = self.audio.shared.out_sample_rate.max(1) as f32;
        let (a_time, b_time) = if let Some((a,b)) = tab.ab_loop { ((a as f32)/sr, (b as f32)/sr) } else { (0.0, 0.0) };
        let ab_text = if tab.ab_loop.is_some() { format!("A: {}  B: {}", crate::app::helpers::format_time_s(a_time), crate::app::helpers::format_time_s(b_time)) } else { "A: --  B: --".to_string() };
        ui.label(RichText::new(ab_text).monospace());
        if ui.button("Set A").on_hover_text("Set A at playhead (A key)").clicked() {
            let pos_now = self.audio.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed);
            let b = tab.ab_loop.map(|(_,b)| b).unwrap_or(pos_now);
            tab.ab_loop = Some((pos_now, b));
            if tab.loop_mode == LoopMode::Marker { let (s,e)= if pos_now<=b {(pos_now,b)} else {(b,pos_now)}; self.audio.set_loop_enabled(true); self.audio.set_loop_region(s,e); }
        }
        if ui.button("Set B").on_hover_text("Set B at playhead (B key)").clicked() {
            let pos_now = self.audio.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed);
            let a = tab.ab_loop.map(|(a,_)| a).unwrap_or(pos_now);
            tab.ab_loop = Some((a, pos_now));
            if tab.loop_mode == LoopMode::Marker { let (s,e)= if a<=pos_now {(a,pos_now)} else {(pos_now,a)}; self.audio.set_loop_enabled(true); self.audio.set_loop_region(s,e); }
        }
        if ui.button("Clear A/B").clicked() { tab.ab_loop = None; apply_pending_loop = true; }
        ui.separator();
        // View mode toggles
        for (vm, label) in [ (ViewMode::Waveform, "Wave"), (ViewMode::Spectrogram, "Spec"), (ViewMode::Mel, "Mel") ] {
            if ui.selectable_label(tab.view_mode == vm, label).clicked() { tab.view_mode = vm; }
        }
        ui.separator();
        // Time HUD: play position / total length
        let pos = self.audio.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed) as f32 / sr as f32;
        let len = (tab.samples_len as f32 / sr as f32).max(0.0);
        ui.label(RichText::new(format!("Pos: {} / {}", crate::app::helpers::format_time_s(pos), crate::app::helpers::format_time_s(len))).monospace());
    });
    ui.separator();

    let avail = ui.available_size();
                // pending actions to perform after UI borrows end
                let mut do_set_loop_from: Option<(usize,usize)> = None;
                let mut do_trim: Option<(usize,usize)> = None;
                let mut do_fade: Option<((usize,usize), f32, f32)> = None;
                // Split canvas and inspector: right panel fixed width
                let inspector_w = 260.0f32;
                let canvas_w = (avail.x - inspector_w).max(100.0);
                ui.horizontal(|ui| {
                    // Canvas area
                    ui.vertical(|ui| {
                        let canvas_h = (canvas_w * 0.35).clamp(180.0, avail.y);
                        let (resp, painter) = ui.allocate_painter(egui::vec2(canvas_w, canvas_h), Sense::click_and_drag());
                        let rect = resp.rect;
                        let w = rect.width().max(1.0); let h = rect.height().max(1.0);
                        painter.rect_filled(rect, 0.0, Color32::from_rgb(16,16,18));
                        // Layout parameters
                        let gutter_w = 44.0;
                        let wave_left = rect.left() + gutter_w;
                        let wave_w = (w - gutter_w).max(1.0);
                        let ch_n = tab.ch_samples.len().max(1);
                        let lane_h = h / ch_n as f32;

                        // Visual amplitude scale: assume Volume=0 dB for display; apply per-file Gain only
                        let gain_db = *self.pending_gains.get(&tab.path).unwrap_or(&0.0);
                        let scale = db_to_amp(gain_db);

                        // Initialize zoom to fit if unset (show whole file)
                        if tab.samples_len > 0 && tab.samples_per_px <= 0.0 {
                            let fit_spp = (tab.samples_len as f32 / wave_w.max(1.0)).max(0.01);
                            tab.samples_per_px = fit_spp;
                            tab.view_offset = 0;
                        }

                // Time ruler (ticks + labels) across all lanes
                {
                    let spp = tab.samples_per_px.max(0.0001);
                    let vis = (wave_w * spp).ceil() as usize;
                    let start = tab.view_offset.min(tab.samples_len);
                    let end = (start + vis).min(tab.samples_len);
                    if end > start {
                        let sr = self.audio.shared.out_sample_rate.max(1) as f32;
                        let t0 = start as f32 / sr;
                        let t1 = end as f32 / sr;
                        let px_per_sec = (1.0 / spp) * sr;
                        let min_px = 90.0;
                        let steps: [f32; 15] = [0.01,0.02,0.05,0.1,0.2,0.5,1.0,2.0,5.0,10.0,15.0,30.0,60.0,120.0,300.0];
                        let mut step = steps[steps.len()-1];
                        for s in steps { if px_per_sec * s >= min_px { step = s; break; } }
                        let start_tick = (t0 / step).floor() * step;
                        let fid = TextStyle::Monospace.resolve(ui.style());
                        let grid_col = Color32::from_rgb(38,38,44);
                        let label_col = Color32::GRAY;
                        let mut t = start_tick;
                        while t <= t1 + step*0.5 {
                            let s_idx = (t * sr).round() as isize;
                            let rel = (s_idx.max(start as isize) - start as isize) as f32;
                            let x = wave_left + (rel / spp).clamp(0.0, wave_w);
                            painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(1.0, grid_col));
                            // Label near top; avoid overcrowding by skipping when too dense
                            if px_per_sec * step >= 70.0 {
                                let label = crate::app::helpers::format_time_s(t);
                                painter.text(egui::pos2(x + 2.0, rect.top() + 2.0), egui::Align2::LEFT_TOP, label, fid.clone(), label_col);
                            }
                            t += step;
                        }
                    }
                }

                // Handle interactions (seek, zoom, pan, selection)
                // Detect hover using pointer position against our canvas rect (robust across senses)
                let pointer_over_canvas = ui.input(|i| i.pointer.hover_pos()).map_or(false, |p| rect.contains(p));
                if pointer_over_canvas {
                    // Zoom with Ctrl + wheel (use hovered pos over this widget)
                    // Combine raw wheel delta with low-level events as a fallback (covers trackpads/pinch, some platforms).
                    let wheel_raw = ui.input(|i| i.raw_scroll_delta);
                    let mut wheel = wheel_raw;
                    let mut pinch_zoom_factor: f32 = 1.0;
                    let events = ctx.input(|i| i.events.clone());
                    for ev in events {
                        match ev {
                            egui::Event::Scroll(delta) => { wheel += delta; }
                            egui::Event::Zoom(z) => { pinch_zoom_factor *= z; }
                            _ => {}
                        }
                    }
                    let scroll_y = wheel.y;
                    let modifiers = ui.input(|i| i.modifiers);
                    let pointer_pos = resp.hover_pos();
                    // Debug trace (dev builds): log incoming deltas and modifiers when over canvas
                    #[cfg(debug_assertions)]
                    if wheel_raw != egui::Vec2::ZERO || pinch_zoom_factor != 1.0 {
                        eprintln!(
                            "wheel_raw=({:.2},{:.2}) wheel_total=({:.2},{:.2}) ctrl={} shift={} pinch={:.3}",
                            wheel_raw.x, wheel_raw.y, wheel.x, wheel.y, modifiers.ctrl, modifiers.shift, pinch_zoom_factor
                        );
                    }
                    // Zoom: plain wheel (unless Shift is held for pan) or pinch zoom
                    if (((scroll_y.abs() > 0.0) && !modifiers.shift) || (pinch_zoom_factor != 1.0)) && tab.samples_len > 0 {
                        // Wheel up = zoom in
                        let factor = if pinch_zoom_factor != 1.0 { pinch_zoom_factor } else if scroll_y < 0.0 { 0.9 } else { 1.1 };
                        let factor = factor.clamp(0.2, 5.0);
                        let old_spp = tab.samples_per_px.max(0.0001);
                        let cursor_x = pointer_pos.map(|p| p.x).unwrap_or(wave_left + wave_w * 0.5).clamp(wave_left, wave_left + wave_w);
                        let t = ((cursor_x - wave_left) / wave_w).clamp(0.0, 1.0);
                        let vis = (wave_w * old_spp).ceil() as usize;
                        let anchor = tab.view_offset.saturating_add((t * vis as f32) as usize).min(tab.samples_len);
                        // Dynamic clamp: allow full zoom-out to "fit whole"
                        let min_spp = 0.01; // 100 px per sample
                        let max_spp_fit = (tab.samples_len as f32 / wave_w.max(1.0)).max(min_spp);
                        tab.samples_per_px = (old_spp * factor).clamp(min_spp, max_spp_fit);
                        let vis2 = (wave_w * tab.samples_per_px).ceil() as usize;
                        let left = anchor.saturating_sub((t * vis2 as f32) as usize);
                        let max_left = tab.samples_len.saturating_sub(vis2);
                        tab.view_offset = left.min(max_left);
                    }
                    // Pan with Shift + wheel (prefer horizontal wheel if available)
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
                    // Pan with Middle / Right drag, or Alt + Left drag (DAW-like)
                    let (left_down, mid_down, right_down, alt_mod) = ui.input(|i| (
                        i.pointer.button_down(egui::PointerButton::Primary),
                        i.pointer.button_down(egui::PointerButton::Middle),
                        i.pointer.button_down(egui::PointerButton::Secondary),
                        i.modifiers.alt,
                    ));
                    let alt_left_pan = alt_mod && left_down;
                    if (mid_down || right_down || alt_left_pan) && tab.samples_len > 0 {
                        let dx = ui.input(|i| i.pointer.delta().x);
                        if dx.abs() > 0.0 {
                            let delta = (-dx * tab.samples_per_px) as isize;
                            let mut off = tab.view_offset as isize + delta;
                            let vis = (wave_w * tab.samples_per_px).ceil() as usize;
                            let max_left = tab.samples_len.saturating_sub(vis);
                            if off < 0 { off = 0; }
                            if off as usize > max_left { off = max_left as isize; }
                            tab.view_offset = off as usize;
                        }
                    }
                }
                // Selection vs Seek with primary button (Alt+LeftDrag = pan handled above)
                let alt_now = ui.input(|i| i.modifiers.alt);
                if !alt_now {
                    // Dragging: create/update selection
                    if resp.dragged_by(egui::PointerButton::Primary) {
                        if tab.drag_select_anchor.is_none() {
                            if let Some(pos) = resp.interact_pointer_pos() {
                                let x = pos.x.clamp(wave_left, wave_left + wave_w);
                                let spp = tab.samples_per_px.max(0.0001);
                                let vis = (wave_w * spp).ceil() as usize;
                                let s0 = tab.view_offset.saturating_add((((x - wave_left) / wave_w) * vis as f32) as usize).min(tab.samples_len);
                                tab.drag_select_anchor = Some(s0);
                            }
                        }
                        if let (Some(anchor), Some(pos)) = (tab.drag_select_anchor, resp.interact_pointer_pos()) {
                            let x = pos.x.clamp(wave_left, wave_left + wave_w);
                            let spp = tab.samples_per_px.max(0.0001);
                            let vis = (wave_w * spp).ceil() as usize;
                            let s1 = tab.view_offset.saturating_add((((x - wave_left) / wave_w) * vis as f32) as usize).min(tab.samples_len);
                            let (a,b) = if anchor<=s1 { (anchor,s1) } else { (s1,anchor) };
                            tab.selection = Some((a,b));
                        }
                    }
                    // Drag release: finalize selection (optional zero-cross snap)
                    if resp.drag_released() {
                        if let (true, Some((mut a,mut b))) = (tab.snap_zero_cross, tab.selection) {
                            if a < b && tab.samples_len > 2 {
                                let mono = self.audio.shared.samples.load();
                                if let Some(buf) = mono.as_ref() {
                                    let find_zero = |mut idx: usize, dir: isize| -> usize {
                                        let n = buf.len();
                                        let mut i = idx as isize;
                                        let step = if dir>=0 { 1 } else { -1 };
                                        let mut last = buf[idx].clamp(-1.0,1.0);
                                        for _ in 0..2048 { // limited search window
                                            i += step;
                                            if i <= 0 || i as usize >= n { break; }
                                            let v = buf[i as usize].clamp(-1.0,1.0);
                                            if last.signum() != v.signum() { return i.max(0) as usize; }
                                            last = v;
                                        }
                                        idx
                                    };
                                    a = find_zero(a, -1);
                                    b = find_zero(b.saturating_sub(1), 1);
                                    if a < b { tab.selection = Some((a,b)); }
                                }
                            }
                        }
                        tab.drag_select_anchor = None;
                    }
                    // Click without drag: seek
                    if resp.clicked_by(egui::PointerButton::Primary) {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            let x = pos.x.clamp(wave_left, wave_left + wave_w);
                            let spp = tab.samples_per_px.max(0.0001);
                            let vis = (wave_w * spp).ceil() as usize;
                            let seek = tab.view_offset.saturating_add((((x - wave_left) / wave_w) * vis as f32) as usize).min(tab.samples_len);
                            self.audio.seek_to_sample(seek);
                        }
                    }
                    // Double-click: select whole buffer
                    if resp.double_clicked() {
                        tab.selection = Some((0, tab.samples_len));
                    }
                }

                // Draw selection overlay and AB markers (shared across lanes)
                {
                    // selection band
                    if let Some((s,e)) = tab.selection {
                        if e > s {
                            let spp = tab.samples_per_px.max(0.0001);
                            let x0 = wave_left + (((s.saturating_sub(tab.view_offset)) as f32 / spp).clamp(0.0, wave_w));
                            let x1 = wave_left + (((e.saturating_sub(tab.view_offset)) as f32 / spp).clamp(0.0, wave_w));
                            let sel_rect = egui::Rect::from_min_max(egui::pos2(x0.min(x1), rect.top()), egui::pos2(x0.max(x1), rect.bottom()));
                            painter.rect_filled(sel_rect, 0.0, Color32::from_rgba_unmultiplied(80,120,200,60));
                        }
                    }
                    // A/B markers and labels
                    if let Some((a,b)) = tab.ab_loop {
                        let spp = tab.samples_per_px.max(0.0001);
                        let to_x = |samp: usize| wave_left + (((samp.saturating_sub(tab.view_offset)) as f32 / spp).clamp(0.0, wave_w));
                        let ax = to_x(a); let bx = to_x(b);
                        let st = egui::Stroke::new(2.0, Color32::from_rgb(60,160,255));
                        painter.line_segment([egui::pos2(ax, rect.top()), egui::pos2(ax, rect.bottom())], st);
                        painter.line_segment([egui::pos2(bx, rect.top()), egui::pos2(bx, rect.bottom())], st);
                        let fid = TextStyle::Monospace.resolve(ui.style());
                        painter.text(egui::pos2(ax + 4.0, rect.top() + 4.0), egui::Align2::LEFT_TOP, "A", fid.clone(), Color32::from_rgb(170,200,255));
                        painter.text(egui::pos2(bx + 4.0, rect.top() + 4.0), egui::Align2::LEFT_TOP, "B", fid, Color32::from_rgb(170,200,255));
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
                    let vis = (wave_w * spp).ceil() as usize; // samples in view
                    let start = tab.view_offset.min(tab.samples_len);
                    let end = (start + vis).min(tab.samples_len);
                    let visible_len = end.saturating_sub(start);
                    if visible_len > 0 {
                        // Two rendering paths depending on zoom level:
                        // - Aggregated min/max bins for spp >= 1.0 (>= 1 sample per pixel)
                        // - Direct per-sample polyline/stem for spp < 1.0 (< 1 sample per pixel)
                        if spp >= 1.0 {
                            let bins = wave_w as usize; // one bin per pixel
                    if bins > 0 {
                        let mut tmp = Vec::new();
                        build_minmax(&mut tmp, &ch[start..end], bins);
                        let n = tmp.len().max(1) as f32;
                        for (idx, &(mn, mx)) in tmp.iter().enumerate() {
                            let mn = (mn * scale).clamp(-1.0, 1.0);
                            let mx = (mx * scale).clamp(-1.0, 1.0);
                            let x = lane_rect.left() + (idx as f32 / n) * wave_w;
                            let y0 = lane_rect.center().y - mx * (lane_rect.height()*0.48);
                            let y1 = lane_rect.center().y - mn * (lane_rect.height()*0.48);
                            let amp = (mn.abs().max(mx.abs())).clamp(0.0, 1.0);
                            let col = amp_to_color(amp);
                            painter.line_segment([egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))], egui::Stroke::new(1.0, col));
                        }
                    }
                } else {
                    // Fine zoom: draw per-sample. When there are fewer samples than pixels,
                    // distribute samples evenly across the available width and connect them.
                    let scale_y = lane_rect.height() * 0.48;
                    if visible_len == 1 {
                        let sx = lane_rect.left() + wave_w * 0.5;
                        let v = (ch[start] * scale).clamp(-1.0, 1.0);
                        let sy = lane_rect.center().y - v * scale_y;
                        let col = amp_to_color(v.abs().clamp(0.0, 1.0));
                        painter.circle_filled(egui::pos2(sx, sy), 2.0, col);
                    } else {
                        let denom = (visible_len - 1) as f32;
                        let mut last: Option<(f32, f32, egui::Color32)> = None;
                        for (i, &v0) in ch[start..end].iter().enumerate() {
                            let v = (v0 * scale).clamp(-1.0, 1.0);
                            let t = (i as f32) / denom;
                            let sx = lane_rect.left() + t * wave_w;
                            let sy = lane_rect.center().y - v * scale_y;
                            let col = amp_to_color(v.abs().clamp(0.0, 1.0));
                            if let Some((px, py, pc)) = last {
                                // Use previous color to avoid color flicker between segments
                                painter.line_segment([egui::pos2(px, py), egui::pos2(sx, sy)], egui::Stroke::new(1.0, pc));
                            }
                            last = Some((sx, sy, col));
                        }
                        // Optionally draw stems for clarity when pixels-per-sample is large
                        let pps = 1.0 / spp; // pixels per sample
                        if pps >= 6.0 {
                            for (i, &v0) in ch[start..end].iter().enumerate() {
                                let v = (v0 * scale).clamp(-1.0, 1.0);
                                let t = (i as f32) / denom;
                                let sx = lane_rect.left() + t * wave_w;
                                let sy = lane_rect.center().y - v * scale_y;
                                let base = lane_rect.center().y;
                                let col = amp_to_color(v.abs().clamp(0.0, 1.0));
                                painter.line_segment([egui::pos2(sx, base), egui::pos2(sx, sy)], egui::Stroke::new(1.0, col));
                            }
                        }
                    }
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
                        // Playhead time label
                        let sr_f = self.audio.shared.out_sample_rate.max(1) as f32;
                        let pos_time = (pos as f32) / sr_f;
                        let label = crate::app::helpers::format_time_s(pos_time);
                        let fid = TextStyle::Monospace.resolve(ui.style());
                        let text_pos = egui::pos2(x + 6.0, rect.top() + 2.0);
                        painter.text(text_pos, egui::Align2::LEFT_TOP, label, fid, Color32::from_rgb(180, 200, 220));
                    }
                }
                    }); // end canvas UI

                    // Inspector area (right)
                    ui.vertical(|ui| {
                        ui.set_width(inspector_w);
                        ui.heading("Inspector");
                        ui.separator();
                        match tab.view_mode {
                            ViewMode::Waveform => {
                                ui.label(RichText::new(format!("Tool: {:?}", tab.active_tool)).strong());
                                match tab.active_tool {
                                    ToolKind::SeekSelect => {
                                        if let Some((s,e)) = tab.selection { ui.label(format!("Selection: {}  E{} samp", s, e)); }
                                        else { ui.label("Selection: (none)"); }
                                    }
                                    ToolKind::LoopEdit => {
                                        let (a,b) = tab.ab_loop.unwrap_or((0,0));
                                        ui.label(format!("A: {}  B: {} samp", a, b));
                                        if ui.button("Use selection as loop").clicked() {
                                            if let Some((s,e)) = Self::editor_selected_range(tab) { do_set_loop_from = Some((s,e)); }
                                        }
                                        if ui.button("Clear A/B").clicked() { do_set_loop_from = Some((0,0)); }
                                    }
                                    ToolKind::Trim => {
                                        ui.label("Trim to Selection or A–B");
                                        if ui.button("Trim Now").clicked() { if let Some((s,e)) = Self::editor_selected_range(tab) { do_trim = Some((s,e)); } }
                                    }
                                    ToolKind::Fade => {
                                        let st = tab.tool_state;
                                        let mut in_ms = st.fade_in_ms; let mut out_ms = st.fade_out_ms;
                                        ui.label("Fade In (ms)"); ui.add(egui::DragValue::new(&mut in_ms).clamp_range(0.0..=10000.0).speed(5.0));
                                        ui.label("Fade Out (ms)"); ui.add(egui::DragValue::new(&mut out_ms).clamp_range(0.0..=10000.0).speed(5.0));
                                        // write back into tab state
                                        tab.tool_state = ToolState{ fade_in_ms: in_ms, fade_out_ms: out_ms };
                                        if ui.button("Apply Fade to Selection/A–B/Whole").clicked() {
                                            let range = Self::editor_selected_range(tab).unwrap_or((0, tab.samples_len));
                                            do_fade = Some((range, in_ms, out_ms));
                                        }
                                        if ui.button("Quick: Edge XFade (ms)").clicked() {
                                            let range = Self::editor_selected_range(tab).unwrap_or((0, tab.samples_len));
                                            let ms = in_ms.max(out_ms).max(5.0);
                                            do_fade = Some((range, ms, ms));
                                        }
                                    }
                                    ToolKind::Gain | ToolKind::Normalize => {
                                        ui.label("Planned in next step.");
                                    }
                                }
                            }
                            _ => { ui.label("Tools for this view will appear here."); }
                        }
                    }); // end inspector
                }); // end horizontal split

                // perform pending actions after borrows end
                if let Some((s,e)) = do_set_loop_from { if let Some(tab) = self.tabs.get_mut(tab_idx) { if s==0 && e==0 { tab.ab_loop=None; } else { tab.ab_loop=Some((s,e)); if tab.loop_enabled { self.audio.set_loop_region(s,e); } } } }
                if let Some((s,e)) = do_trim { self.editor_apply_trim_range(tab_idx, (s,e)); }
                if let Some(((s,e), in_ms, out_ms)) = do_fade { self.editor_apply_fade_range(tab_idx, (s,e), in_ms, out_ms); }
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
                    .column(egui_extras::Column::initial(90.0).resizable(true))      // Level (original)
                    .column(egui_extras::Column::initial(90.0).resizable(true))      // LUFS (Integrated)
                    .column(egui_extras::Column::initial(80.0).resizable(true))      // Gain (editable)
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
                    header.col(|ui| { sort_changed |= sortable_header(ui, "dBFS (Peak)", &mut self.sort_key, &mut self.sort_dir, SortKey::Level, false); });
                    header.col(|ui| { sort_changed |= sortable_header(ui, "LUFS (I)", &mut self.sort_key, &mut self.sort_dir, SortKey::Lufs, false); });
                    header.col(|ui| { ui.label(RichText::new("Gain (dB)").strong()); });
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
                        let is_selected = self.selected_multi.contains(&row_idx);
                        row.set_selected(is_selected);

                        if is_data {
                            let path_owned = self.files[row_idx].clone();
                            let name = path_owned.file_name().and_then(|s| s.to_str()).unwrap_or("(invalid)");
                            let parent = path_owned.parent().and_then(|p| p.to_str()).unwrap_or("");
                            let mut clicked_to_load = false;
                            let mut clicked_to_select = false;
                            // Ensure quick header meta is present when row is shown
                            if !self.meta.contains_key(&path_owned) {
                                if let Ok(reader) = hound::WavReader::open(&path_owned) {
                                    let spec = reader.spec();
                                    self.meta.insert(path_owned.clone(), FileMeta { channels: spec.channels, sample_rate: spec.sample_rate, bits_per_sample: spec.bits_per_sample, duration_secs: None, rms_db: None, peak_db: None, lufs_i: None, thumb: Vec::new() });
                                }
                            }
                            let meta = self.meta.get(&path_owned).cloned();

                            // col 0: File (clickable label with clipping)
                            row.col(|ui| {
                                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                    let mark = if self.pending_gains.get(&path_owned).map(|v| v.abs() > 0.0001).unwrap_or(false) { " •" } else { "" };
                                    let resp = ui.add(
                                        egui::Label::new(RichText::new(format!("{}{}", name, mark)).size(text_height * 1.05))
                                            .sense(Sense::click())
                                            .truncate(true)
                                    ).on_hover_cursor(egui::CursorIcon::PointingHand);
                                    
                                    // シングルクリチE��: 行選抁E+ 音声ロード（後段で一括処琁E��E                                    if resp.clicked() && !resp.double_clicked() { clicked_to_load = true; }
                                    
                                    // ダブルクリチE��: エチE��タタブで開く
                                    if resp.double_clicked() { clicked_to_select = true; to_open = Some(path_owned.clone()); }
                                    
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
                                    
                                    // シングルクリチE��: 行選抁E+ 音声ローチE                                    if resp.clicked() && !resp.double_clicked() { clicked_to_load = true; }
                                    
                                    // ダブルクリチE��: シスチE��のファイルブラウザでフォルダを開く！EAVファイルを選択状態で�E�E                                    if resp.double_clicked() { clicked_to_select = true; let _ = open_folder_with_file_selected(&path_owned); }
                                    
                                    if resp.hovered() {
                                        resp.on_hover_text(parent);
                                    }
                                });
                            });
                            // col 2: Length (mm:ss) - clickable
                            row.col(|ui| {
                                let secs = meta.as_ref().and_then(|m| m.duration_secs).unwrap_or(f32::NAN);
                                let text = if secs.is_finite() { format_duration(secs) } else { "...".into() };
                                let resp = ui.add(
                                    egui::Label::new(RichText::new(text).monospace())
                                        .sense(Sense::click())
                                ).on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked() { clicked_to_load = true; }
                            });
                            // col 3: Channels - clickable
                            row.col(|ui| {
                                let ch = meta.as_ref().map(|m| m.channels).unwrap_or(0);
                                let resp = ui.add(
                                    egui::Label::new(RichText::new(format!("{}", ch)).monospace())
                                        .sense(Sense::click())
                                ).on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked() { clicked_to_load = true; }
                            });
                            // col 4: Sample rate - clickable
                            row.col(|ui| {
                                let sr = meta.as_ref().map(|m| m.sample_rate).unwrap_or(0);
                                let resp = ui.add(
                                    egui::Label::new(RichText::new(format!("{}", sr)).monospace())
                                        .sense(Sense::click())
                                ).on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked() { clicked_to_load = true; }
                            });
                            // col 5: Bits per sample - clickable
                            row.col(|ui| {
                                let bits = meta.as_ref().map(|m| m.bits_per_sample).unwrap_or(0);
                                let resp = ui.add(
                                    egui::Label::new(RichText::new(format!("{}", bits)).monospace())
                                        .sense(Sense::click())
                                ).on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked() { clicked_to_load = true; }
                            });
                            // col 6: dBFS (Peak) with Gain反映�E�Eolumeは含めなぁE��E clickable
                            row.col(|ui| {
                                let (rect2, resp2) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::click());
                                let gain_db = *self.pending_gains.get(&path_owned).unwrap_or(&0.0);
                                let orig = meta.as_ref().and_then(|m| m.peak_db);
                                let adj = orig.map(|db| db + gain_db);
                                if let Some(db) = adj { ui.painter().rect_filled(rect2, 4.0, db_to_color(db)); }
                                let text = adj.map(|db| format!("{:.1}", db)).unwrap_or_else(|| "...".into());
                                let fid = TextStyle::Monospace.resolve(ui.style());
                                ui.painter().text(rect2.center(), egui::Align2::CENTER_CENTER, text, fid, Color32::WHITE);
                                if resp2.clicked() { clicked_to_load = true; }
                                // (optional tooltip removed to avoid borrow and unused warnings)
                            });
                            // col 7: LUFS (Integrated) with background color (same palette as dBFS)
                            row.col(|ui| {
                                let base = meta.as_ref().and_then(|m| m.lufs_i);
                                let gain_db = *self.pending_gains.get(&path_owned).unwrap_or(&0.0);
                                let eff = if let Some(v) = self.lufs_override.get(&path_owned) { Some(*v) } else { base.map(|v| v + gain_db) };
                                let (rect2, resp2) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::click());
                                if let Some(db) = eff { ui.painter().rect_filled(rect2, 4.0, db_to_color(db)); }
                                let text = eff.map(|v| format!("{:.1}", v)).unwrap_or_else(|| "...".into());
                                let fid = TextStyle::Monospace.resolve(ui.style());
                                ui.painter().text(rect2.center(), egui::Align2::CENTER_CENTER, text, fid, Color32::WHITE);
                                if resp2.clicked() { clicked_to_load = true; }
                            });
                            // col 8: Gain (dB) editable
                            row.col(|ui| {
                                let old = *self.pending_gains.get(&path_owned).unwrap_or(&0.0);
                                let mut g = old;
                                let resp = ui.add(
                                    egui::DragValue::new(&mut g)
                                        .clamp_range(-24.0..=24.0)
                                        .speed(0.1)
                                        .fixed_decimals(1)
                                        .suffix(" dB")
                                );
                                if resp.changed() {
                                    let new = Self::clamp_gain_db(g);
                                    let delta = new - old;
                                    if self.selected_multi.len() > 1 && self.selected_multi.contains(&row_idx) {
                                        let indices = self.selected_multi.clone();
                                        self.adjust_gain_for_indices(&indices, delta);
                                    } else {
                                        if new == 0.0 { self.pending_gains.remove(&path_owned); } else { self.pending_gains.insert(path_owned.clone(), new); }
                                        if self.playing_path.as_ref() == Some(&path_owned) { self.apply_effective_volume(); }
                                    }
                                    // schedule LUFS recompute (debounced)
                                    self.schedule_lufs_for_path(path_owned.clone());
                                }
                            });
                            // col 9: Wave thumbnail - clickable
                            row.col(|ui| {
                                let desired_w = ui.available_width().max(80.0);
                                let thumb_h = (desired_w * 0.22).clamp(text_height * 1.2, text_height * 4.0);
                                let (rect, painter) = ui.allocate_painter(egui::vec2(desired_w, thumb_h), Sense::click());
                                if row_idx == 0 { self.wave_row_h = thumb_h; }
                                if let Some(m) = meta.as_ref() { let w = rect.rect.width(); let h = rect.rect.height(); let n = m.thumb.len().max(1) as f32; 
                                        let gain_db = *self.pending_gains.get(&path_owned).unwrap_or(&0.0);
                                        let scale = db_to_amp(gain_db);
                                        for (idx, &(mn0, mx0)) in m.thumb.iter().enumerate() {
                                        let mn = (mn0 * scale).clamp(-1.0, 1.0);
                                        let mx = (mx0 * scale).clamp(-1.0, 1.0);
                                        let x = rect.rect.left() + (idx as f32 / n) * w; let y0 = rect.rect.center().y - mx * (h*0.45); let y1 = rect.rect.center().y - mn * (h*0.45);
                                        let a = (mn.abs().max(mx.abs())).clamp(0.0,1.0);
                                        let col = amp_to_color(a);
                                        painter.line_segment([egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))], egui::Stroke::new(1.0, col)); } }
                                if rect.clicked() { clicked_to_load = true; }
                            });
                            // col 10: Spacer (fills remainder so scrollbar stays at right edge)
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); });

                            // Row-level click handling (background/any non-interactive area)
                            let resp = row.response();
                            if resp.clicked() { clicked_to_load = true; }
                            if is_selected && self.scroll_to_selected { resp.scroll_to_me(Some(Align::Center)); }
                            if clicked_to_load {
                                // multi-select aware selection update (read modifiers from ctx to avoid UI borrow conflict)
                                let mods = ctx.input(|i| i.modifiers);
                                self.update_selection_on_click(row_idx, mods);
                                // load clicked row regardless of modifiers
                                self.select_and_load(row_idx);
                            } else if clicked_to_select { self.selected = Some(row_idx); self.scroll_to_selected = true; self.selected_multi.clear(); self.selected_multi.insert(row_idx); self.select_anchor = Some(row_idx); }
                        } else {
                            // filler row to extend frame
                            row.col(|_ui| {});
                            row.col(|_ui| {});
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // Length
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // Ch
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // SR
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // Bits
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // Level
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // LUFS
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // Gain
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
        if activate_path.is_none() {
            if let Some(pending) = self.pending_activate_path.take() { activate_path = Some(pending); }
        }
        if let Some(p) = activate_path {
            // Reload audio for the activated tab only; do not touch stored waveform
            match self.mode {
                RateMode::Speed => { let _ = prepare_for_speed(&p, &self.audio, &mut Vec::new(), self.playback_rate); self.audio.set_rate(self.playback_rate); }
                _ => { self.audio.set_rate(1.0); self.spawn_heavy_processing(&p); }
            }
            if let Some(idx) = self.active_tab { if let Some(tab) = self.tabs.get(idx) { self.audio.set_loop_enabled(tab.loop_enabled); if let Some(buf) = self.audio.shared.samples.load().as_ref() { self.audio.set_loop_region(0, buf.len()); } } }
            // Update effective volume to include per-file gain for the activated tab
            self.apply_effective_volume();
        }
        // Clear pending scroll flag after building the table
        self.scroll_to_selected = false;

        // Busy overlay (blocks input and shows loader)
        if self.processing.is_some() || self.export_state.is_some() {
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
                        let msg = if let Some(p) = &self.processing { p.msg.as_str() } else if let Some(st)=&self.export_state { st.msg.as_str() } else { "Working..." };
                        ui.label(RichText::new(msg).strong());
                    });
                });
            });
        }
        ctx.request_repaint_after(Duration::from_millis(16));
        
        // Leave dirty editor confirmation
        if self.show_leave_prompt {
            egui::Window::new("Leave Editor?").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0,0.0)).show(ctx, |ui| {
                ui.label("The waveform has been modified in memory. Leave this editor?");
                ui.horizontal(|ui| {
                    if ui.button("Leave").clicked() {
                        match self.leave_intent.take() {
                            Some(LeaveIntent::CloseTab(i)) => {
                                if i < self.tabs.len() { self.tabs.remove(i); if let Some(ai)=self.active_tab { if ai==i { self.active_tab=None; } else if ai>i { self.active_tab=Some(ai-1); } } }
                                self.audio.stop();
                            }
                            Some(LeaveIntent::ToTab(i)) => {
                                if let Some(t) = self.tabs.get(i) { self.active_tab = Some(i); self.audio.stop(); self.pending_activate_path = Some(t.path.clone()); } self.rebuild_current_buffer_with_mode();
                            }
                            Some(LeaveIntent::ToList) => { self.active_tab=None; self.audio.stop(); self.audio.set_loop_enabled(false); }
                            None => {}
                        }
                        self.show_leave_prompt = false;
                    }
                    if ui.button("Cancel").clicked() { self.leave_intent=None; self.show_leave_prompt=false; }
                });
            });
        }
        
        // First save prompt window
        if self.show_first_save_prompt {
            egui::Window::new("First Save Option").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0,0.0)).show(ctx, |ui| {
                ui.label("Choose default save behavior for Ctrl+S:");
                ui.horizontal(|ui| {
                    if ui.button("Overwrite").clicked() {
                        self.export_cfg.save_mode = SaveMode::Overwrite;
                        self.export_cfg.first_prompt = false;
                        self.show_first_save_prompt = false;
                        self.trigger_save_selected();
                    }
                    if ui.button("New File").clicked() {
                        self.export_cfg.save_mode = SaveMode::NewFile;
                        self.export_cfg.first_prompt = false;
                        self.show_first_save_prompt = false;
                        self.trigger_save_selected();
                    }
                    if ui.button("Cancel").clicked() { self.show_first_save_prompt = false; }
                });
            });
        }

        // Export settings window (in separate UI module)
        self.ui_export_settings_window(ctx);
    }
}











