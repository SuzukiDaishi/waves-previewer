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
mod editor_ops;
mod ui;
mod render;
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
        // Heavy preview worker for Pitch/Stretch (mono)
        heavy_preview_rx: Option<std::sync::mpsc::Receiver<Vec<f32>>>,
        heavy_preview_tool: Option<ToolKind>,
        // Heavy overlay worker (per-channel preview for Pitch/Stretch) with generation guard
        heavy_overlay_rx: Option<std::sync::mpsc::Receiver<(std::path::PathBuf, Vec<Vec<f32>>, u64)>>,
        overlay_gen_counter: u64,
        overlay_expected_gen: u64,
    }

impl WavesPreviewer {
    fn preview_restore_audio_for_tab(&self, tab_idx: usize) {
        if let Some(tab) = self.tabs.get(tab_idx) {
            // Rebuild mono from current destructive state
            let mono = Self::editor_mixdown_mono(tab);
            self.audio.set_samples(std::sync::Arc::new(mono));
            // Reapply loop mode
            self.apply_loop_mode_for_tab(tab);
        }
    }
    fn set_preview_mono(&mut self, tab_idx: usize, tool: ToolKind, mono: Vec<f32>) {
        self.audio.set_samples(std::sync::Arc::new(mono));
        if let Some(tab) = self.tabs.get_mut(tab_idx) { tab.preview_audio_tool = Some(tool); }
        if let Some(tab) = self.tabs.get(tab_idx) { self.apply_loop_mode_for_tab(tab); }
    }
    fn clear_preview_if_any(&mut self, tab_idx: usize) {
        if let Some(tab) = self.tabs.get(tab_idx) {
            if tab.preview_audio_tool.is_some() {
                let _ = tab;
                self.preview_restore_audio_for_tab(tab_idx);
                if let Some(tabm) = self.tabs.get_mut(tab_idx) { tabm.preview_audio_tool = None; tabm.preview_overlay_ch = None; }
            }
        }
        // also discard any in-flight heavy preview job
        self.heavy_preview_rx = None;
        self.heavy_preview_tool = None;
    }
    fn spawn_heavy_preview_owned(&mut self, mono: Vec<f32>, tool: ToolKind, param: f32) {
        use std::sync::mpsc;
        let sr = self.audio.shared.out_sample_rate;
        // cancel previous job by dropping receiver
        self.heavy_preview_rx = None; self.heavy_preview_tool = None;
        let (tx, rx) = mpsc::channel::<Vec<f32>>();
        std::thread::spawn(move || {
            let out = match tool {
                ToolKind::PitchShift => crate::wave::process_pitchshift_offline(&mono, sr, sr, param),
                ToolKind::TimeStretch => crate::wave::process_timestretch_offline(&mono, sr, sr, param),
                _ => mono,
            };
            let _ = tx.send(out);
        });
        self.heavy_preview_rx = Some(rx);
        self.heavy_preview_tool = Some(tool);
    }

    // Spawn per-channel overlay generator (Pitch/Stretch) in a worker thread.
    // Note: Call this ONLY after UI borrows end (see E0499 note) to avoid nested &mut self borrows.
    fn spawn_heavy_overlay_for_tab(&mut self, tab_idx: usize, tool: ToolKind, param: f32) {
        use std::sync::mpsc;
        // Cancel previous overlay job by dropping receiver
        self.heavy_overlay_rx = None;
        if let Some(tab) = self.tabs.get(tab_idx) {
            let path = tab.path.clone();
            let ch = tab.ch_samples.clone();
            let sr = self.audio.shared.out_sample_rate;
            // generation guard
            self.overlay_gen_counter = self.overlay_gen_counter.wrapping_add(1);
            let gen = self.overlay_gen_counter;
            self.overlay_expected_gen = gen;
            let (tx, rx) = mpsc::channel::<(std::path::PathBuf, Vec<Vec<f32>>, u64)>();
            std::thread::spawn(move || {
                let mut out: Vec<Vec<f32>> = Vec::with_capacity(ch.len());
                for chan in ch.iter() {
                    let processed = match tool {
                        ToolKind::PitchShift => crate::wave::process_pitchshift_offline(chan, sr, sr, param),
                        ToolKind::TimeStretch => crate::wave::process_timestretch_offline(chan, sr, sr, param),
                        _ => chan.clone(),
                    };
                    out.push(processed);
                }
                let _ = tx.send((path, out, gen));
            });
            self.heavy_overlay_rx = Some(rx);
        }
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
    // editor operations moved to editor_ops.rs
    fn apply_loop_mode_for_tab(&self, tab: &EditorTab) {
        match tab.loop_mode {
            LoopMode::Off => { self.audio.set_loop_enabled(false); }
            LoopMode::OnWhole => {
                self.audio.set_loop_enabled(true);
                if let Some(buf) = self.audio.shared.samples.load().as_ref() {
                    let len = buf.len();
                    self.audio.set_loop_region(0, len);
                    let cf = tab.loop_xfade_samples.min(len / 2);
                    self.audio.set_loop_crossfade(cf, match tab.loop_xfade_shape { crate::app::types::LoopXfadeShape::Linear => 0, crate::app::types::LoopXfadeShape::EqualPower => 1 });
                }
            }
            LoopMode::Marker => {
                if let Some((a,b)) = tab.loop_region { if a!=b { let (s,e) = if a<=b {(a,b)} else {(b,a)}; self.audio.set_loop_enabled(true); self.audio.set_loop_region(s,e); let cf = tab.loop_xfade_samples.min((e.saturating_sub(s)) / 2); self.audio.set_loop_crossfade(cf, match tab.loop_xfade_shape { crate::app::types::LoopXfadeShape::Linear => 0, crate::app::types::LoopXfadeShape::EqualPower => 1 }); return; } }
                self.audio.set_loop_enabled(false);
            }
        }
    }
    #[allow(dead_code)]
    fn set_marker_sample(tab: &mut EditorTab, idx: usize) {
        match tab.loop_region {
            None => tab.loop_region = Some((idx, idx)),
            Some((a,b)) => {
                if a==b { tab.loop_region = Some((a.min(idx), a.max(idx))); }
                else { let da = a.abs_diff(idx); let db = b.abs_diff(idx); if da <= db { tab.loop_region = Some((idx, b)); } else { tab.loop_region = Some((a, idx)); } }
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
            heavy_preview_rx: None,
            heavy_preview_tool: None,
            heavy_overlay_rx: None,
            overlay_gen_counter: 0,
            overlay_expected_gen: 0,

        })
    }

}

impl eframe::App for WavesPreviewer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Update meter from audio RMS (approximate dBFS)
        {
            let rms = self.audio.shared.meter_rms.load(std::sync::atomic::Ordering::Relaxed);
            let db = if rms > 0.0 { 20.0 * rms.max(1e-8).log10() } else { -80.0 };
            self.meter_db = db.clamp(-80.0, 6.0);
        }
        // Ensure effective volume (global vol x per-file gain) is always applied
        self.apply_effective_volume();
        // Drain heavy preview results
        if let Some(rx) = &self.heavy_preview_rx {
            if let Ok(mono) = rx.try_recv() {
                if let Some(idx) = self.active_tab { if let Some(tool) = self.heavy_preview_tool { self.set_preview_mono(idx, tool, mono); } }
                self.heavy_preview_rx = None; self.heavy_preview_tool = None;
            }
        }
        // Drain heavy per-channel overlay results
        if let Some(rx) = &self.heavy_overlay_rx {
            if let Ok((p, overlay, gen)) = rx.try_recv() {
                if gen == self.overlay_expected_gen {
                    if let Some(idx) = self.tabs.iter().position(|t| t.path == p) {
                        if let Some(tab) = self.tabs.get_mut(idx) { tab.preview_overlay_ch = Some(overlay); }
                    }
                }
                self.heavy_overlay_rx = None;
            }
        }
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
        // Editor-specific shortcuts: Loop region setters, Loop toggle (L), Zero-cross snap (S)
        if let Some(tab_idx) = self.active_tab {
            // Loop Start/End at playhead
            if ctx.input(|i| i.key_pressed(Key::K)) { // Set Loop Start
                let pos_now = self.audio.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed);
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    let end = tab.loop_region.map(|(_,e)| e).unwrap_or(pos_now);
                    let s = pos_now.min(end);
                    let e = end.max(s);
                    tab.loop_region = Some((s,e));
                }
            }
            if ctx.input(|i| i.key_pressed(Key::P)) { // Set Loop End
                let pos_now = self.audio.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed);
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    let start = tab.loop_region.map(|(s,_)| s).unwrap_or(pos_now);
                    let s = start.min(pos_now);
                    let e = pos_now.max(start);
                    tab.loop_region = Some((s,e));
                }
            }
            if ctx.input(|i| i.key_pressed(Key::L)) {
                // Toggle loop mode without holding a mutable borrow across &self call
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.loop_mode = match tab.loop_mode { LoopMode::Off => LoopMode::OnWhole, _ => LoopMode::Off };
                }
                if let Some(tab_ro) = self.tabs.get(tab_idx) {
                    self.apply_loop_mode_for_tab(tab_ro);
                }
            }
            if ctx.input(|i| i.key_pressed(Key::S)) {
                if let Some(tab) = self.tabs.get_mut(tab_idx) { tab.snap_zero_cross = !tab.snap_zero_cross; }
            }
        }
        
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
        // Drag & Drop: merge dropped files/folders into the list (WAV only)
        {
            let dropped: Vec<egui::DroppedFile> = ctx.input(|i| i.raw.dropped_files.clone());
            if !dropped.is_empty() {
                let mut paths: Vec<std::path::PathBuf> = Vec::new();
                for f in dropped {
                    if let Some(p) = f.path { paths.push(p); }
                }
                if !paths.is_empty() {
                    let added = self.add_files_merge(&paths);
                    if added > 0 { self.after_add_refresh(); }
                }
            }
        }
        let mut activate_path: Option<PathBuf> = None;
        egui::CentralPanel::default().show(ctx, |ui| {            // Tabs
            ui.horizontal_wrapped(|ui| {
                let is_list = self.active_tab.is_none();
                let list_label = if is_list { RichText::new("[List]").strong() } else { RichText::new("List") };
                if ui.selectable_label(is_list, list_label).clicked() {
                    if let Some(idx) = self.active_tab { self.clear_preview_if_any(idx); }
                    self.active_tab = None;
                    self.audio.stop();
                    self.audio.set_loop_enabled(false);
                }
                let mut to_close: Option<usize> = None;
                let tabs_len = self.tabs.len();
                for i in 0..tabs_len {
                    // avoid holding immutable borrow over calls that mutate self inside closure
                    let active = self.active_tab == Some(i);
                    let display = self.tabs[i].display_name.clone();
                    let path_for_activate = self.tabs[i].path.clone();
                    let text = if active { RichText::new(format!("[{}]", display)).strong() } else { RichText::new(display) };
                    ui.horizontal(|ui| {
                        if ui.selectable_label(active, text).clicked() {
                            // Leaving previous tab: discard any un-applied preview
                            if let Some(prev) = self.active_tab { if prev != i { self.clear_preview_if_any(prev); } }
                            // mutate self safely here
                            self.active_tab = Some(i);
                            activate_path = Some(path_for_activate.clone());
                            self.audio.stop();
                        }
                        if ui.button("x").on_hover_text("Close").clicked() {
                            self.clear_preview_if_any(i);
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
    // Pre-read audio values to avoid borrowing self while editing tab
    let sr_ctx = self.audio.shared.out_sample_rate.max(1) as f32;
    let pos_ctx_now = self.audio.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed);
    let mut request_seek: Option<usize> = None;
    ui.horizontal(|ui| {
        let tab = &mut self.tabs[tab_idx];
        let dirty_mark = if tab.dirty { " •" } else { "" };
        ui.label(RichText::new(format!("{}{}", tab.path.display(), dirty_mark)).monospace());
        ui.separator();
        // Loop mode toggles (kept): Off / OnWhole / Marker
        ui.label("Loop:");
        for (m,label) in [ (LoopMode::Off, "Off"), (LoopMode::OnWhole, "On"), (LoopMode::Marker, "Marker") ] {
            if ui.selectable_label(tab.loop_mode == m, label).clicked() {
                tab.loop_mode = m;
                apply_pending_loop = true;
            }
        }
        ui.separator();
        // View mode toggles
        for (vm, label) in [ (ViewMode::Waveform, "Wave"), (ViewMode::Spectrogram, "Spec"), (ViewMode::Mel, "Mel") ] {
            if ui.selectable_label(tab.view_mode == vm, label).clicked() { tab.view_mode = vm; }
        }
        ui.separator();
        // Time HUD: play position (editable) / total length
        let sr = sr_ctx; // restore local sample-rate alias after removing top-level Loop block
        let mut pos_sec = pos_ctx_now as f32 / sr as f32;
        let len_sec = (tab.samples_len as f32 / sr as f32).max(0.0);
        ui.label("Pos:");
        let pos_resp = ui.add(
            egui::DragValue::new(&mut pos_sec)
                .clamp_range(0.0..=len_sec)
                .speed(0.05)
                .fixed_decimals(2)
        );
        if pos_resp.changed() { let samp = (pos_sec.max(0.0) * sr) as usize; request_seek = Some(samp.min(tab.samples_len)); }
        ui.label(RichText::new(format!(" / {}", crate::app::helpers::format_time_s(len_sec))).monospace());
    });
    ui.separator();

    let avail = ui.available_size();
                // pending actions to perform after UI borrows end
                let mut do_set_loop_from: Option<(usize,usize)> = None;
                let mut do_trim: Option<(usize,usize)> = None; // keep-only (optional)
                let do_fade: Option<((usize,usize), f32, f32)> = None; // legacy whole-file fade
                let mut do_gain: Option<((usize,usize), f32)> = None;
                let mut do_normalize: Option<((usize,usize), f32)> = None;
                let mut do_reverse: Option<(usize,usize)> = None;
                // let mut do_silence: Option<(usize,usize)> = None; // removed
                let mut do_cutjoin: Option<(usize,usize)> = None;
                let mut do_apply_xfade: bool = false;
                let mut do_fade_in: Option<((usize,usize), crate::app::types::FadeShape)> = None;
                let mut do_fade_out: Option<((usize,usize), crate::app::types::FadeShape)> = None;
                // Snapshot busy state and prepare deferred overlay job.
                // IMPORTANT: Do NOT call `self.*` (which takes &mut self) while holding `let tab = &mut self.tabs[...]`.
                // That pattern triggers borrow checker error E0499. Defer such calls to after the UI closures.
                let overlay_busy = self.heavy_overlay_rx.is_some();
                let mut pending_overlay_job: Option<(ToolKind, f32)> = None;
                // Split canvas and inspector: right panel fixed width
                let inspector_w = 300.0f32;
                let canvas_w = (avail.x - inspector_w).max(100.0);
                ui.horizontal(|ui| {
                    let tab = &mut self.tabs[tab_idx];
                    // Canvas area
                    let mut need_restore_preview = false;
                    // Accumulate non-destructive preview audio to audition.
                    // Carry the tool kind to keep preview state consistent.
                    let mut pending_preview: Option<(ToolKind, Vec<f32>)> = None;
                    let mut pending_heavy_preview: Option<(ToolKind, Vec<f32>, f32)> = None;
                    let mut pending_pitch_apply: Option<f32> = None;
                    let mut pending_stretch_apply: Option<f32> = None;
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
                        let new_spp = (old_spp * factor).clamp(min_spp, max_spp_fit);
                        tab.samples_per_px = new_spp;
                        let vis2 = (wave_w * tab.samples_per_px).ceil() as usize;
                        let left = anchor.saturating_sub((t * vis2 as f32) as usize);
                        let max_left = tab.samples_len.saturating_sub(vis2);
                        let new_view = left.min(max_left);
                        #[cfg(debug_assertions)]
                        {
                            let mode = if tab.samples_per_px >= 1.0 { "agg" } else { "line" };
                            let fit_whole = (new_spp - max_spp_fit).abs() < 1e-6;
                            eprintln!(
                                "ZOOM change: spp {:.5} -> {:.5} ({mode}) factor {:.3} vis={} -> {} anchor={} new_view={} wave_w={:.1} fit_whole={}",
                                old_spp, new_spp, factor, vis, vis2, anchor, new_view, wave_w, fit_whole
                            );
                        }
                        tab.view_offset = new_view;
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
                    // Primary interactions: click to seek (no range selection)
                    if resp.clicked_by(egui::PointerButton::Primary) {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            let x = pos.x.clamp(wave_left, wave_left + wave_w);
                            let spp = tab.samples_per_px.max(0.0001);
                            let vis = (wave_w * spp).ceil() as usize;
                            let pos_samp = tab.view_offset.saturating_add((((x - wave_left) / wave_w) * vis as f32) as usize).min(tab.samples_len);
                            request_seek = Some(pos_samp);
                        }
                    }
                }

                // Draw loop region markers (shared across lanes)
                {
                    // no selection band (range selection removed)
                    // Loop region markers and labels (LoopEdit only)
                    if tab.active_tool == ToolKind::LoopEdit { if let Some((a,b)) = tab.loop_region {
                        let spp = tab.samples_per_px.max(0.0001);
                        let to_x = |samp: usize| wave_left + (((samp.saturating_sub(tab.view_offset)) as f32 / spp).clamp(0.0, wave_w));
                        let ax = to_x(a); let bx = to_x(b);
                        let st = egui::Stroke::new(2.0, Color32::from_rgb(60,160,255));
                        painter.line_segment([egui::pos2(ax, rect.top()), egui::pos2(ax, rect.bottom())], st);
                        painter.line_segment([egui::pos2(bx, rect.top()), egui::pos2(bx, rect.bottom())], st);
                        let fid = TextStyle::Monospace.resolve(ui.style());
                        painter.text(egui::pos2(ax + 4.0, rect.top() + 4.0), egui::Align2::LEFT_TOP, "S", fid.clone(), Color32::from_rgb(170,200,255));
                        painter.text(egui::pos2(bx + 4.0, rect.top() + 4.0), egui::Align2::LEFT_TOP, "E", fid, Color32::from_rgb(170,200,255));

                        // When LoopEdit is active, visualize crossfade spans and shapes + boundary fades
                        if tab.active_tool == ToolKind::LoopEdit {
                            let len = b.saturating_sub(a);
                            let cf = tab.loop_xfade_samples.min(len / 2);
                            if cf > 0 {
                                let xs0 = to_x(a);
                                let xs1 = to_x(a + cf);
                                let xe0 = to_x(b.saturating_sub(cf));
                                let xe1 = to_x(b);
                                // shaded bands
                                let col_in = Color32::from_rgba_unmultiplied(255, 180, 60, 40);
                                let col_out = Color32::from_rgba_unmultiplied(60, 180, 255, 40);
                                let r_in = egui::Rect::from_min_max(egui::pos2(xs0, rect.top()), egui::pos2(xs1, rect.bottom()));
                                let r_out = egui::Rect::from_min_max(egui::pos2(xe0, rect.top()), egui::pos2(xe1, rect.bottom()));
                                painter.rect_filled(r_in, 0.0, col_in);
                                painter.rect_filled(r_out, 0.0, col_out);

                                // draw shape curves (orange) for intuition
                                let curve_col = Color32::from_rgb(255, 170, 60);
                                let steps = 48;
                                let mut last_in: Option<egui::Pos2> = None;
                                let mut last_out: Option<egui::Pos2> = None;
                                let h = rect.height();
                                let y_of = |w: f32| rect.bottom() - w * h; // map weight to y
                                for i in 0..=steps {
                                    let t = (i as f32) / (steps as f32);
                                    let (w_out, w_in) = match tab.loop_xfade_shape {
                                        crate::app::types::LoopXfadeShape::EqualPower => {
                                            let a = core::f32::consts::FRAC_PI_2 * t; (a.cos(), a.sin())
                                        }
                                        crate::app::types::LoopXfadeShape::Linear => (1.0 - t, t),
                                    };
                                    // incoming (start side): left band
                                    let x_in = egui::lerp(xs0..=xs1, t);
                                    let p_in = egui::pos2(x_in, y_of(w_in));
                                    if let Some(lp) = last_in { painter.line_segment([lp, p_in], egui::Stroke::new(2.0, curve_col)); }
                                    last_in = Some(p_in);
                                    // outgoing (end side): right band
                                    let x_out = egui::lerp(xe0..=xe1, t);
                                    let p_out = egui::pos2(x_out, y_of(w_out));
                                    if let Some(lp) = last_out { painter.line_segment([lp, p_out], egui::Stroke::new(2.0, curve_col)); }
                                    last_out = Some(p_out);
                                }
                            }
                        }
                    }}
                    // Trim A/B markers (only in Trim tool)
                    if tab.active_tool == ToolKind::Trim { if let Some((a,b)) = tab.trim_range { if b> a {
                        let spp = tab.samples_per_px.max(0.0001);
                        let to_x = |samp: usize| wave_left + (((samp.saturating_sub(tab.view_offset)) as f32 / spp).clamp(0.0, wave_w));
                        let ax = to_x(a); let bx = to_x(b);
                        let st = egui::Stroke::new(2.0, Color32::from_rgb(255,140,0));
                        painter.line_segment([egui::pos2(ax, rect.top()), egui::pos2(ax, rect.bottom())], st);
                        painter.line_segment([egui::pos2(bx, rect.top()), egui::pos2(bx, rect.bottom())], st);
                        let fid = TextStyle::Monospace.resolve(ui.style());
                        painter.text(egui::pos2(ax + 4.0, rect.top() + 4.0), egui::Align2::LEFT_TOP, "A", fid.clone(), Color32::from_rgb(255,200,150));
                        painter.text(egui::pos2(bx + 4.0, rect.top() + 4.0), egui::Align2::LEFT_TOP, "B", fid, Color32::from_rgb(255,200,150));
                        // shaded band between A and B for clarity
                        let r = egui::Rect::from_min_max(egui::pos2(ax, rect.top()), egui::pos2(bx, rect.bottom()));
                        painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(255,160,60,32));
                    } }}
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
                            // Aggregated mode: also draw overlay here so it shows at widest zoom
                            if tab.active_tool != ToolKind::Trim && tab.preview_overlay_ch.is_some() {
                                if let Some(overlay) = &tab.preview_overlay_ch {
                                    let och: Option<&[f32]> = overlay.get(ci).map(|v| v.as_slice()).or_else(|| overlay.get(0).map(|v| v.as_slice()));
                                    if let Some(buf) = och {
                                        use crate::app::render::overlay as ov;
                                        use crate::app::render::colors::{OVERLAY_COLOR, OVERLAY_STROKE_BASE, OVERLAY_STROKE_EMPH};
                                        let orig_total = tab.samples_len.max(1);
                                        let (startb, _endb, over_vis) = ov::map_visible_overlay(start, visible_len, orig_total, buf.len());
                                        if over_vis > 0 {
                                            let bins = wave_w as usize;
                                            let bins_values = ov::compute_overlay_bins_for_base_columns(start, visible_len, startb, over_vis, buf, bins);
                                            // Draw full overlay
                                            ov::draw_bins_locked(&painter, lane_rect, wave_w, &bins_values, scale, OVERLAY_COLOR, OVERLAY_STROKE_BASE);
                                            // Emphasize LoopEdit boundary segments if applicable
                                            if tab.active_tool == ToolKind::LoopEdit {
                                                if let Some((a, b)) = tab.loop_region {
                                                    let len = b.saturating_sub(a);
                                                    let cf = tab.loop_xfade_samples.min(len / 2).min(tab.samples_len);
                                                    if cf > 0 {
                                                        // Map original boundary segments into overlay domain using ratio
                                                        let ratio = (buf.len().max(1) as f32) / (orig_total as f32);
                                                        let a0 = (((a as f32) * ratio).round() as usize).min(buf.len());
                                                        let a1 = (((a as f32 + cf as f32) * ratio).round() as usize).min(buf.len());
                                                        let b0 = (((b as f32 - cf as f32) * ratio).round() as usize).min(buf.len());
                                                        let b1 = (((b as f32) * ratio).round() as usize).min(buf.len());
                                                        let segs = [(a0, a1), (b0, b1)];
                                                        for (s, e) in segs {
                                                            if let Some((p0, p1)) = ov::overlay_px_range_for_segment(startb, over_vis, bins, s, e) {
                                                                if p1 > p0 && p1 <= bins {
                                                                    let span_left = lane_rect.left() + (p0 as f32 / bins as f32) * wave_w;
                                                                    let span_w = ((p1 - p0) as f32 / bins as f32) * wave_w;
                                                                    let span_rect = egui::Rect::from_min_size(egui::pos2(span_left, lane_rect.top()), egui::vec2(span_w, lane_rect.height()));
                                                                    let sub = &bins_values[p0..p1];
                                                                    ov::draw_bins_in_rect(&painter, span_rect, sub, scale, OVERLAY_COLOR, OVERLAY_STROKE_EMPH);
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
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

                    // Overlay preview aligned to this lane (if any), per-channel.
                    // Skip Trim tool (Trim does not show green overlay by spec).
                    // Draw whenever overlay data is present to avoid relying on preview_audio_tool state.
                    #[cfg(debug_assertions)]
                    {
                        let mode = if spp >= 1.0 { "agg" } else { "line" };
                        let has_ov = tab.preview_overlay_ch.is_some();
                        eprintln!(
                            "OVERLAY gate: mode={} has_overlay={} active={:?} spp={:.5} vis_len={} start={} end={} view_off={} len={}",
                            mode, has_ov, tab.active_tool, spp, visible_len, start, end, tab.view_offset, tab.samples_len
                        );
                    }
                    if tab.active_tool != ToolKind::Trim && tab.preview_overlay_ch.is_some() {
                        if let Some(overlay) = &tab.preview_overlay_ch {
                            // try channel match, fallback to first channel if overlay is mono
                            let och: Option<&[f32]> = overlay.get(ci).map(|v| v.as_slice()).or_else(|| overlay.get(0).map(|v| v.as_slice()));
                            if let Some(buf) = och {
                                // Map original-visible [start,end) to overlay domain using length ratio.
                                // This keeps overlays visible at any zoom, even when length differs (e.g. TimeStretch).
                                let lenb = buf.len();
                                let orig_total = tab.samples_len.max(1);
                                let ratio = (lenb as f32) / (orig_total as f32);
                                let orig_vis = visible_len.max(1);
                                // Map visible window [start .. start+orig_vis) into overlay domain using total-length ratio
                                // Align overlay start to original start using nearest sample to minimize off-by-one drift
                                let startb = (((start as f32) * ratio).round() as usize).min(lenb);
                                let mut endb = startb + (((orig_vis as f32) * ratio).ceil() as usize);
                                if endb > lenb { endb = lenb; }
                                if startb >= endb { endb = (startb + 1).min(lenb); }
                                let over_vis = (endb.saturating_sub(startb)).max(1);
                                let r_w = (over_vis as f32) / (orig_vis as f32);
                                let ov_w = (wave_w * r_w).max(1.0);
                                #[cfg(debug_assertions)]
                                {
                                    let mode = if spp >= 1.0 { "agg" } else { "line" };
                                    eprintln!(
                                        "OVERLAY map: mode={} lenb={} startb={} endb={} over_vis={} ov_w_px={:.1}",
                                        mode, lenb, startb, endb, over_vis, ov_w
                                    );
                                }
                                if startb < endb {
                                    // Pre-compute LoopEdit highlight segments (mapped to overlay domain)
                                    let (seg1_opt, seg2_opt) = if tab.active_tool == ToolKind::LoopEdit {
                                        if let Some((a, b)) = tab.loop_region {
                                            let len = b.saturating_sub(a);
                                            let cf = tab.loop_xfade_samples.min(len / 2).min(tab.samples_len);
                                            if cf > 0 {
                                                let a0 = (((a as f32) * ratio).round() as usize).min(lenb);
                                                let a1 = (((a as f32 + cf as f32) * ratio).round() as usize).min(lenb);
                                                let b0 = (((b as f32 - cf as f32) * ratio).round() as usize).min(lenb);
                                                let b1 = (((b as f32) * ratio).round() as usize).min(lenb);
                                                let s1 = a0.max(startb); let e1 = a1.min(endb);
                                                let s2 = b0.max(startb); let e2 = b1.min(endb);
                                                (if s1 < e1 { Some((s1,e1)) } else { None }, if s2 < e2 { Some((s2,e2)) } else { None })
                                            } else { (None, None) }
                                        } else { (None, None) }
                                    } else { (None, None) };

                                    // helper: draw polyline for [p0,p1) within [startb,endb) mapped into [0..ov_w]
                                    let _draw_segment_poly = |p0: usize, p1: usize| {
                                        let seg_len = p1.saturating_sub(p0);
                                        if seg_len == 0 { return; }
                                        let seg_ratio = (seg_len as f32) / (over_vis as f32);
                                        let seg_w = (ov_w * seg_ratio).max(1.0);
                                        let seg_x0 = lane_rect.left() + ((p0 - startb) as f32 / over_vis as f32) * ov_w;
                                        let count = seg_w.max(1.0) as usize; // ~1 point per px
                                        let denom = (count.saturating_sub(1)).max(1) as f32;
                                        let scale_y = lane_rect.height() * 0.48;
                                        #[cfg(debug_assertions)]
                                        {
                                            let band = egui::Rect::from_min_max(egui::pos2(seg_x0, lane_rect.top()), egui::pos2(seg_x0 + seg_w, lane_rect.bottom()));
                                            painter.rect_filled(band, 0.0, Color32::from_rgba_unmultiplied(110, 255, 200, 20));
                                            eprintln!(
                                                "OVERLAY seg: p0={} p1={} seg_len={} seg_w_px={:.1} count={}",
                                                p0, p1, seg_len, seg_w, count
                                            );
                                        }
                                        // Widest zoom: a very short segment can quantize to <=1px. Ensure something is drawn.
                                        if count <= 2 {
                                            let idx = p0; // head of segment as representative
                                            let v = (buf[idx] * scale).clamp(-1.0, 1.0);
                                            let sx = seg_x0 + (seg_w * 0.5);
                                            let sy = lane_rect.center().y - v * scale_y;
                                            // Draw a short tick so it remains visible
                                            let tick_h = (lane_rect.height() * 0.10).max(2.0);
                                            painter.line_segment(
                                                [egui::pos2(sx, sy - tick_h*0.5), egui::pos2(sx, sy + tick_h*0.5)],
                                                egui::Stroke::new(1.8, Color32::from_rgb(80, 240, 160))
                                            );
                                            #[cfg(debug_assertions)]
                                            eprintln!("OVERLAY seg: fallback_tick used at x={:.1}", sx);
                                            return;
                                        }
                                        let mut last: Option<egui::Pos2> = None;
                                        for i in 0..count {
                                            let t = (i as f32) / denom;
                                            let idx = p0 + ((t * (seg_len as f32 - 1.0)).round() as usize).min(seg_len - 1);
                                            let v = (buf[idx] * scale).clamp(-1.0, 1.0);
                                            let sx = seg_x0 + t * seg_w;
                                            let sy = lane_rect.center().y - v * scale_y;
                                            let p = egui::pos2(sx, sy);
                                            if let Some(lp) = last { painter.line_segment([lp, p], egui::Stroke::new(1.8, Color32::from_rgb(80, 240, 160))); }
                                            last = Some(p);
                                        }
                                    };

                                    if spp >= 1.0 {
                                        // Aggregated: compute bins via helper and draw pixel-locked bars
                                        let bins = wave_w as usize;
                                        if bins > 0 {
                                            let ratio_approx_1 = (over_vis as i64 - orig_vis as i64).abs() <= 1;
                                            let values = if ratio_approx_1 {
                                                let mut tmp = Vec::new();
                                                build_minmax(&mut tmp, &buf[start..end], bins);
                                                tmp
                                            } else {
                                                crate::app::render::overlay::compute_overlay_bins_for_base_columns(
                                                    start, orig_vis, startb, over_vis, buf, bins
                                                )
                                            };
                                            crate::app::render::overlay::draw_bins_locked(
                                                &painter, lane_rect, wave_w, &values, scale, egui::Color32::from_rgb(80, 240, 160), 1.3
                                            );
                                        }
                                        // Emphasize LoopEdit boundary subranges if present (thicker over the same px columns)
                                        if let Some((s1,e1)) = seg1_opt {
                                            let bins = wave_w as usize;
                                            if bins > 0 {
                                                let step_b = (orig_vis as f32) / (bins as f32);
                                                let mut pos_b = 0.0f32;
                                                let px_end = ((over_vis as f32 / orig_vis as f32) * bins as f32).round().clamp(1.0, bins as f32) as usize;
                                                for px in 0..px_end {
                                                    let i0 = start + pos_b.floor() as usize;
                                                    pos_b += step_b;
                                                    let mut i1 = start + pos_b.floor() as usize;
                                                    if i1 <= i0 { i1 = i0 + 1; }
                                                    let mut o0 = startb + (((i0 - start) as f32 * over_vis as f32 / orig_vis as f32).round() as usize);
                                                    let mut o1 = startb + (((i1 - start) as f32 * over_vis as f32 / orig_vis as f32).round() as usize);
                                                    if o1 <= o0 { o1 = o0 + 1; }
                                                    o0 = o0.max(s1); o1 = o1.min(e1);
                                                    if o1 <= o0 { continue; }
                                                    let mut mn = f32::INFINITY; let mut mx = f32::NEG_INFINITY;
                                                    for &v in &buf[o0..o1] { if v < mn { mn = v; } if v > mx { mx = v; } }
                                                    if !mn.is_finite() || !mx.is_finite() { continue; }
                                                    let mn = (mn * scale).clamp(-1.0, 1.0);
                                                    let mx = (mx * scale).clamp(-1.0, 1.0);
                                                    let x = lane_rect.left() + (px as f32 / bins as f32) * wave_w;
                                                    let y0 = lane_rect.center().y - mx * (lane_rect.height()*0.48);
                                                    let y1 = lane_rect.center().y - mn * (lane_rect.height()*0.48);
                                                    painter.line_segment([egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))], egui::Stroke::new(1.6, Color32::from_rgb(80, 240, 160)));
                                                }
                                            }
                                        }
                                        if let Some((s2,e2)) = seg2_opt {
                                            let bins = wave_w as usize;
                                            if bins > 0 {
                                                let step_b = (orig_vis as f32) / (bins as f32);
                                                let mut pos_b = 0.0f32;
                                                let px_end = ((over_vis as f32 / orig_vis as f32) * bins as f32).round().clamp(1.0, bins as f32) as usize;
                                                for px in 0..px_end {
                                                    let i0 = start + pos_b.floor() as usize;
                                                    pos_b += step_b;
                                                    let mut i1 = start + pos_b.floor() as usize;
                                                    if i1 <= i0 { i1 = i0 + 1; }
                                                    let mut o0 = startb + (((i0 - start) as f32 * over_vis as f32 / orig_vis as f32).round() as usize);
                                                    let mut o1 = startb + (((i1 - start) as f32 * over_vis as f32 / orig_vis as f32).round() as usize);
                                                    if o1 <= o0 { o1 = o0 + 1; }
                                                    o0 = o0.max(s2); o1 = o1.min(e2);
                                                    if o1 <= o0 { continue; }
                                                    let mut mn = f32::INFINITY; let mut mx = f32::NEG_INFINITY;
                                                    for &v in &buf[o0..o1] { if v < mn { mn = v; } if v > mx { mx = v; } }
                                                    if !mn.is_finite() || !mx.is_finite() { continue; }
                                                    let mn = (mn * scale).clamp(-1.0, 1.0);
                                                    let mx = (mx * scale).clamp(-1.0, 1.0);
                                                    let x = lane_rect.left() + (px as f32 / bins as f32) * wave_w;
                                                    let y0 = lane_rect.center().y - mx * (lane_rect.height()*0.48);
                                                    let y1 = lane_rect.center().y - mn * (lane_rect.height()*0.48);
                                                    painter.line_segment([egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))], egui::Stroke::new(1.6, Color32::from_rgb(80, 240, 160)));
                                                }
                                            }
                                        }
                                    } else {
                                        let denom = (endb - startb - 1).max(1) as f32;
                                        let scale_y = lane_rect.height() * 0.48;
                                        #[cfg(debug_assertions)]
                                        {
                                            let x0 = lane_rect.left();
                                            let x1 = x0 + ov_w;
                                            let band = egui::Rect::from_min_max(egui::pos2(x0, lane_rect.top()), egui::pos2(x1, lane_rect.bottom()));
                                            painter.rect_filled(band, 0.0, Color32::from_rgba_unmultiplied(80, 240, 160, 20));
                                        }
                                        let mut last: Option<egui::Pos2> = None;
                                        for i in startb..endb {
                                            let v = (buf[i] * scale).clamp(-1.0, 1.0);
                                            let t = (i - startb) as f32 / denom;
                                            let sx = lane_rect.left() + t * ov_w;
                                            let sy = lane_rect.center().y - v * scale_y;
                                            let p = egui::pos2(sx, sy);
                                            if let Some(lp) = last { painter.line_segment([lp, p], egui::Stroke::new(1.5, Color32::from_rgb(80, 240, 160))); }
                                            last = Some(p);
                                        }
                                        // Add stems like the base waveform when zoomed in enough
                                        let pps = 1.0 / spp; // pixels per sample
                                        if pps >= 6.0 {
                                            for i in startb..endb {
                                                let v = (buf[i] * scale).clamp(-1.0, 1.0);
                                                let t = (i - startb) as f32 / denom;
                                                let sx = lane_rect.left() + t * ov_w;
                                                let sy = lane_rect.center().y - v * scale_y;
                                                let base = lane_rect.center().y;
                                                painter.line_segment([egui::pos2(sx, base), egui::pos2(sx, sy)], egui::Stroke::new(1.0, Color32::from_rgb(80, 240, 160)));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                    }
                }

                // (Removed) global mono overlay to avoid double/triple drawing.

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
                                // Tool selector
                                ui.label("Tool:");
                                let mut tool = tab.active_tool;
                                egui::ComboBox::from_label("")
                                    .selected_text(format!("{:?}", tool))
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut tool, ToolKind::LoopEdit, "Loop Edit");
                                        ui.selectable_value(&mut tool, ToolKind::Trim, "Trim");
                                        ui.selectable_value(&mut tool, ToolKind::Fade, "Fade");
                                        ui.selectable_value(&mut tool, ToolKind::PitchShift, "PitchShift");
                                        ui.selectable_value(&mut tool, ToolKind::TimeStretch, "TimeStretch");
                                        ui.selectable_value(&mut tool, ToolKind::Gain, "Gain");
                                        ui.selectable_value(&mut tool, ToolKind::Normalize, "Normalize");
                                        ui.selectable_value(&mut tool, ToolKind::Reverse, "Reverse");
                                    });
                                if tool != tab.active_tool {
                                    tab.active_tool_last = Some(tab.active_tool);
                                    // Leaving a tool: if we had a runtime preview, restore original audio
                                    if tab.preview_audio_tool.is_some() { need_restore_preview = true; tab.preview_audio_tool = None; }
                                    tab.active_tool = tool;
                                }
                                ui.separator();
                                ui.label(RichText::new(format!("Tool: {:?}", tab.active_tool)).strong());
                                match tab.active_tool {
                                    // Seek/Select removed: seeking is always available on the canvas
                                    ToolKind::LoopEdit => {
                                        // compact spacing for inspector controls
                                        ui.scope(|ui| {
                                            let s = ui.style_mut();
                                            s.spacing.item_spacing = egui::vec2(6.0, 6.0);
                                            s.spacing.button_padding = egui::vec2(6.0, 3.0);
                                            let (s0,e0) = tab.loop_region.unwrap_or((0,0));
                                            ui.label("Loop (samples)");
                                            let mut s_i = s0 as i64;
                                            let mut e_i = e0 as i64;
                                            let max_i = tab.samples_len as i64;
                                            ui.horizontal_wrapped(|ui| {
                                                ui.label("Start:");
                                                let chs = ui.add(egui::DragValue::new(&mut s_i).clamp_range(0..=max_i).speed(64.0)).changed();
                                                ui.label("End:");
                                                let che = ui.add(egui::DragValue::new(&mut e_i).clamp_range(0..=max_i).speed(64.0)).changed();
                                                if chs || che {
                                                    let mut s = s_i.clamp(0, max_i) as usize;
                                                    let mut e = e_i.clamp(0, max_i) as usize;
                                                    if e < s { std::mem::swap(&mut s, &mut e); }
                                                    tab.loop_region = Some((s,e));
                                                    apply_pending_loop = true;
                                                }
                                            });
                                            // Crossfade controls (duration in ms + shape)
                                            let sr = self.audio.shared.out_sample_rate.max(1) as f32;
                                            let mut x_ms = (tab.loop_xfade_samples as f32 / sr) * 1000.0;
                                            ui.horizontal_wrapped(|ui| {
                                                ui.label("Xfade (ms):");
                                                if ui.add(egui::DragValue::new(&mut x_ms).clamp_range(0.0..=5000.0).speed(5.0).fixed_decimals(1)).changed() {
                                                    let samp = ((x_ms / 1000.0) * sr).round().clamp(0.0, tab.samples_len as f32) as usize;
                                                    tab.loop_xfade_samples = samp;
                                                    apply_pending_loop = true;
                                                }
                                                ui.label("Shape:");
                                                let mut shp = tab.loop_xfade_shape;
                                                egui::ComboBox::from_id_source("xfade_shape").selected_text(match shp { crate::app::types::LoopXfadeShape::Linear => "Linear", crate::app::types::LoopXfadeShape::EqualPower => "Equal" }).show_ui(ui, |ui| {
                                                    ui.selectable_value(&mut shp, crate::app::types::LoopXfadeShape::Linear, "Linear");
                                                    ui.selectable_value(&mut shp, crate::app::types::LoopXfadeShape::EqualPower, "Equal");
                                                });
                                                if shp != tab.loop_xfade_shape { tab.loop_xfade_shape = shp; apply_pending_loop = true; }
                                            });
                                            ui.horizontal_wrapped(|ui| {
                                                if ui.button("Set Start").on_hover_text("Set Start at playhead").clicked() {
                                                    let pos = self.audio.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed).min(tab.samples_len);
                                                    let end = tab.loop_region.map(|(_,e)| e).unwrap_or(pos);
                                                    let (mut s, mut e) = (pos, end);
                                                    if e < s { std::mem::swap(&mut s, &mut e); }
                                                    tab.loop_region = Some((s,e));
                                                    apply_pending_loop = true;
                                                }
                                                if ui.button("Set End").on_hover_text("Set End at playhead").clicked() {
                                                    let pos = self.audio.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed).min(tab.samples_len);
                                                    let start = tab.loop_region.map(|(s,_)| s).unwrap_or(pos);
                                                    let (mut s, mut e) = (start, pos);
                                                    if e < s { std::mem::swap(&mut s, &mut e); }
                                                    tab.loop_region = Some((s,e));
                                                    apply_pending_loop = true;
                                                }
                                                if ui.button("Clear").clicked() { do_set_loop_from = Some((0,0)); }
                                            });

                                            // Crossfade controls already above; add Apply button to destructively bake Xfade
                                            ui.horizontal_wrapped(|ui| {
                                                let len_ok = tab.loop_region.map(|(a,b)| b> a).unwrap_or(false);
                                                let n = tab.loop_xfade_samples;
                                                if ui.add_enabled(len_ok && n>0, egui::Button::new("Apply Xfade")).on_hover_text("Bake crossfade into data at loop boundary").clicked() {
                                                    do_apply_xfade = true;
                                                }
                                            });

                                            // Dynamic preview overlay for LoopEdit (non-destructive):
                                            // Build a mono preview applying the current loop crossfade to the mixdown.
                                            if let Some((a,b)) = tab.loop_region {
                                                let len = b.saturating_sub(a);
                                                let cf = tab.loop_xfade_samples.min(len / 2).min(tab.samples_len);
                                                if cf > 0 {
                                                    // Build per-channel overlay applying crossfade at boundaries
                                                    let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                    let cf_f = cf.max(1) as f32;
                                                    for ch in overlay.iter_mut() {
                                                        for i in 0..cf {
                                                            let head_idx = a.saturating_add(i);
                                                            let tail_idx = b.saturating_sub(cf).saturating_add(i);
                                                            if head_idx >= ch.len() || tail_idx >= ch.len() { break; }
                                                            let t = (i as f32) / cf_f;
                                                            let (w_out, w_in) = match tab.loop_xfade_shape {
                                                                crate::app::types::LoopXfadeShape::EqualPower => {
                                                                    let ang = core::f32::consts::FRAC_PI_2 * t; (ang.cos(), ang.sin())
                                                                }
                                                                crate::app::types::LoopXfadeShape::Linear => (1.0 - t, t),
                                                            };
                                                            let head = ch[head_idx];
                                                            let tail = ch[tail_idx];
                                                            let m = tail * w_out + head * w_in;
                                                            ch[head_idx] = m;
                                                            ch[tail_idx] = m;
                                                        }
                                                    }
                                                    tab.preview_overlay_ch = Some(overlay);
                                                    tab.preview_audio_tool = Some(ToolKind::LoopEdit);
                                                }
                                            }
                                        });
                                    }
                                    
                                    ToolKind::Trim => {
                                        ui.scope(|ui| {
                                            let s = ui.style_mut();
                                            s.spacing.item_spacing = egui::vec2(6.0, 6.0);
                                            s.spacing.button_padding = egui::vec2(6.0, 3.0);
                                            // Trim has its own A/B range (independent from loop)
                                            let range_opt = tab.trim_range;
                                            if let Some((smp,emp)) = range_opt { ui.label(format!("Trim A–B: {}..{} samp", smp, emp)); } else { ui.label("Trim A–B: (set below)"); }
                                            // A/B setters from playhead
                                            ui.horizontal_wrapped(|ui| {
                                                if ui.button("Set A").on_hover_text("Set A at playhead").clicked() {
                                                    let pos = self.audio.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed).min(tab.samples_len);
                                                let new_r = match tab.trim_range { None => Some((pos, pos)), Some((_a,b)) => Some((pos.min(b), pos.max(b))) };
                                                    tab.trim_range = new_r;
                                                    if let Some((a,b)) = tab.trim_range { if b>a {
                                                        // live preview: keep-only A–B
                                                        let mut mono = Self::editor_mixdown_mono(tab);
                                                        mono = mono[a..b].to_vec();
                                                    pending_preview = Some((ToolKind::Trim, mono));
                                                    tab.preview_audio_tool = Some(ToolKind::Trim);
                                                    } }
                                                }
                                                if ui.button("Set B").on_hover_text("Set B at playhead").clicked() {
                                                    let pos = self.audio.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed).min(tab.samples_len);
                                                let new_r = match tab.trim_range { None => Some((pos, pos)), Some((a,_b)) => Some((a.min(pos), a.max(pos))) };
                                                    tab.trim_range = new_r;
                                                    if let Some((a,b)) = tab.trim_range { if b>a {
                                                        let mut mono = Self::editor_mixdown_mono(tab);
                                                        mono = mono[a..b].to_vec();
                                                    pending_preview = Some((ToolKind::Trim, mono));
                                                    tab.preview_audio_tool = Some(ToolKind::Trim);
                                                    } }
                                                }
                                                if ui.button("Clear").clicked() { tab.trim_range = None; need_restore_preview = true; }
                                            });
                                            // Actions
                                            ui.horizontal_wrapped(|ui| {
                                            let dis = !range_opt.map(|(s,e)| e> s).unwrap_or(false);
                                            let range = range_opt.unwrap_or((0,0));
                                            if ui.add_enabled(!dis, egui::Button::new("Cut+Join")).clicked() { do_cutjoin = Some(range); }
                                            if ui.add_enabled(!dis, egui::Button::new("Apply Keep A–B")).clicked() { do_trim = Some(range); tab.preview_audio_tool=None; }
                                        });
                                        });
                                    }
                                    ToolKind::Fade => {
                                        // Simplified: duration (seconds) from start/end + Apply
                                        ui.scope(|ui| {
                                            let s = ui.style_mut();
                                            s.spacing.item_spacing = egui::vec2(6.0, 6.0);
                                            s.spacing.button_padding = egui::vec2(6.0, 3.0);
                                            let sr = self.audio.shared.out_sample_rate.max(1) as f32;
                                            // Fade In
                                            ui.label("Fade In");
                                            ui.horizontal_wrapped(|ui| {
                                                let mut secs = tab.tool_state.fade_in_ms / 1000.0;
                                                ui.label("duration (s)");
                                                let changed = ui.add(egui::DragValue::new(&mut secs).clamp_range(0.0..=600.0).speed(0.05).fixed_decimals(2)).changed();
                                                if changed {
                                                    tab.tool_state = ToolState{ fade_in_ms: (secs*1000.0).max(0.0), ..tab.tool_state };
                                                    // Live preview (per-channel overlay) + mono audition
                                                    let n = ((secs) * sr).round() as usize;
                                                    // Build overlay per channel
                                                    let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                    for ch in overlay.iter_mut() {
                                                        let nn = n.min(ch.len());
                                                        for i in 0..nn { let t = i as f32 / nn.max(1) as f32; let w = Self::fade_weight(tab.fade_in_shape, t); ch[i] *= w; }
                                                    }
                                                    tab.preview_overlay_ch = Some(overlay.clone());
                                                    // Mono audition
                                                    let mut mono = Self::editor_mixdown_mono(tab);
                                                    let nn = n.min(mono.len());
                                                    for i in 0..nn { let t = i as f32 / nn.max(1) as f32; let w = Self::fade_weight(tab.fade_in_shape, t); mono[i] *= w; }
                                                    pending_preview = Some((ToolKind::Fade, mono));
                                                    tab.preview_audio_tool = Some(ToolKind::Fade);
                                                }
                                                if ui.add_enabled(secs>0.0, egui::Button::new("Apply")).clicked() {
                                                    let n = ((secs) * sr).round() as usize;
                                                    do_fade_in = Some(((0, n.min(tab.samples_len)), tab.fade_in_shape));
                                                    tab.preview_audio_tool = None; // will be rebuilt from destructive result below
                                                    tab.preview_overlay_ch = None;
                                                }
                                            });
                                            ui.separator();
                                            // Fade Out
                                            ui.label("Fade Out");
                                            ui.horizontal_wrapped(|ui| {
                                                let mut secs = tab.tool_state.fade_out_ms / 1000.0;
                                                ui.label("duration (s)");
                                                let changed = ui.add(egui::DragValue::new(&mut secs).clamp_range(0.0..=600.0).speed(0.05).fixed_decimals(2)).changed();
                                                if changed {
                                                    tab.tool_state = ToolState{ fade_out_ms: (secs*1000.0).max(0.0), ..tab.tool_state };
                                                    let n = ((secs) * sr).round() as usize;
                                                    // per-channel overlay
                                                    let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                    for ch in overlay.iter_mut() {
                                                        let len = ch.len(); let nn = n.min(len);
                                                        for i in 0..nn { let t = i as f32 / nn.max(1) as f32; let w = 1.0 - Self::fade_weight(tab.fade_out_shape, t); let idx = len - nn + i; ch[idx] *= w; }
                                                    }
                                                    tab.preview_overlay_ch = Some(overlay.clone());
                                                    // mono audition
                                                    let mut mono = Self::editor_mixdown_mono(tab);
                                                    let len = mono.len(); let nn = n.min(len);
                                                    for i in 0..nn { let t = i as f32 / nn.max(1) as f32; let w = 1.0 - Self::fade_weight(tab.fade_out_shape, t); let idx = len - nn + i; mono[idx] *= w; }
                                                    pending_preview = Some((ToolKind::Fade, mono));
                                                    tab.preview_audio_tool = Some(ToolKind::Fade);
                                                }
                                                if ui.add_enabled(secs>0.0, egui::Button::new("Apply")).clicked() {
                                                    let n = ((secs) * sr).round() as usize;
                                                    do_fade_out = Some(((0, n.min(tab.samples_len)), tab.fade_out_shape));
                                                    tab.preview_audio_tool = None;
                                                    tab.preview_overlay_ch = None;
                                                }
                                            });
                                        });
                                    }
                                    ToolKind::PitchShift => {
                                        ui.scope(|ui| {
                                            let s = ui.style_mut(); s.spacing.item_spacing = egui::vec2(6.0,6.0); s.spacing.button_padding = egui::vec2(6.0,3.0);
                                            let mut semi = tab.tool_state.pitch_semitones;
                                            ui.label("Semitones");
                                            let changed = ui.add(egui::DragValue::new(&mut semi).clamp_range(-12.0..=12.0).speed(0.1).fixed_decimals(2)).changed();
                                            if changed {
                                                tab.tool_state = ToolState{ pitch_semitones: semi, ..tab.tool_state };
                                                let mono = Self::editor_mixdown_mono(tab);
                                                pending_heavy_preview = Some((ToolKind::PitchShift, mono, semi));
                                                // Defer overlay spawn to avoid nested &mut borrow
                                                pending_overlay_job = Some((ToolKind::PitchShift, semi));
                                                tab.preview_audio_tool = Some(ToolKind::PitchShift);
                                            }
                                            if overlay_busy { ui.add(egui::Spinner::new()); }
                                            if ui.button("Apply").clicked() { pending_pitch_apply = Some(tab.tool_state.pitch_semitones); }
                                        });
                                    }
                                    ToolKind::TimeStretch => {
                                        ui.scope(|ui| {
                                            let s = ui.style_mut(); s.spacing.item_spacing = egui::vec2(6.0,6.0); s.spacing.button_padding = egui::vec2(6.0,3.0);
                                            let mut rate = tab.tool_state.stretch_rate;
                                            ui.label("Rate");
                                            let changed = ui.add(egui::DragValue::new(&mut rate).clamp_range(0.25..=4.0).speed(0.02).fixed_decimals(2)).changed();
                                            if changed {
                                                tab.tool_state = ToolState{ stretch_rate: rate, ..tab.tool_state };
                                                let mono = Self::editor_mixdown_mono(tab);
                                                pending_heavy_preview = Some((ToolKind::TimeStretch, mono, rate));
                                                // Defer overlay spawn to avoid nested &mut borrow
                                                pending_overlay_job = Some((ToolKind::TimeStretch, rate));
                                                tab.preview_audio_tool = Some(ToolKind::TimeStretch);
                                            }
                                            if overlay_busy { ui.add(egui::Spinner::new()); }
                                            if ui.button("Apply").clicked() { pending_stretch_apply = Some(tab.tool_state.stretch_rate); }
                                        });
                                    }
                                    ToolKind::Gain => {
                                        let st = tab.tool_state;
                                        let mut gain_db = st.gain_db;
                                        ui.label("Gain (dB)"); ui.add(egui::DragValue::new(&mut gain_db).clamp_range(-24.0..=24.0).speed(0.1));
                                        tab.tool_state = ToolState{ gain_db, ..tab.tool_state };
                                        // live preview on change
                                        if (gain_db - st.gain_db).abs() > 1e-6 {
                                            let g = db_to_amp(gain_db);
                                            // per-channel overlay
                                            let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                            for ch in overlay.iter_mut() { for v in ch.iter_mut() { *v *= g; } }
                                            tab.preview_overlay_ch = Some(overlay);
                                            // mono audition
                                            let mut mono = Self::editor_mixdown_mono(tab);
                                            for v in &mut mono { *v *= g; }
                                            pending_preview = Some((ToolKind::Gain, mono));
                                            tab.preview_audio_tool = Some(ToolKind::Gain);
                                        }
                                        if ui.button("Apply").clicked() { do_gain = Some(((0, tab.samples_len), gain_db)); tab.preview_audio_tool=None; tab.preview_overlay_ch=None; }
                                    }
                                    ToolKind::Normalize => {
                                        let st = tab.tool_state;
                                        let mut target_db = st.normalize_target_db;
                                        ui.label("Target dBFS"); ui.add(egui::DragValue::new(&mut target_db).clamp_range(-24.0..=0.0).speed(0.1));
                                        tab.tool_state = ToolState{ normalize_target_db: target_db, ..tab.tool_state };
                                        // live preview: compute gain to reach target (based on current peak)
                                        let mut mono = Self::editor_mixdown_mono(tab);
                                        if !mono.is_empty() {
                                            let mut peak = 0.0f32; for &v in &mono { peak = peak.max(v.abs()); }
                                            if peak > 0.0 {
                                                let g = db_to_amp(target_db) / peak.max(1e-12);
                                                // per-channel overlay
                                                let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                for ch in overlay.iter_mut() { for v in ch.iter_mut() { *v *= g; } }
                                                tab.preview_overlay_ch = Some(overlay);
                                                // mono audition
                                                for v in &mut mono { *v *= g; }
                                                pending_preview = Some((ToolKind::Normalize, mono));
                                                tab.preview_audio_tool = Some(ToolKind::Normalize);
                                            }
                                        }
                                        if ui.button("Apply").clicked() { do_normalize = Some(((0, tab.samples_len), target_db)); tab.preview_audio_tool=None; tab.preview_overlay_ch=None; }
                                    }
                                    ToolKind::Reverse => {
                                        ui.horizontal_wrapped(|ui| {
                                            if ui.button("Preview").clicked() {
                                                // per-channel overlay
                                                let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                for ch in overlay.iter_mut() { ch.reverse(); }
                                                tab.preview_overlay_ch = Some(overlay);
                                                // mono audition
                                                let mut mono = Self::editor_mixdown_mono(tab);
                                                mono.reverse();
                                                pending_preview = Some((ToolKind::Reverse, mono));
                                                tab.preview_audio_tool = Some(ToolKind::Reverse);
                                            }
                                            if ui.button("Apply").clicked() { do_reverse = Some((0, tab.samples_len)); tab.preview_audio_tool=None; tab.preview_overlay_ch=None; }
                                            if ui.button("Cancel").clicked() { need_restore_preview = true; }
                                        });
                                    }
                                }
                                ui.separator();
                                // Export Selection removed (range selection removed)
                            }
                            _ => { ui.label("Tools for this view will appear here."); }
                        }
                    }); // end inspector
                    if let Some((tool, mono, p)) = pending_heavy_preview { self.spawn_heavy_preview_owned(mono, tool, p); }
                    if let Some(semi) = pending_pitch_apply {
                        let sr = self.audio.shared.out_sample_rate;
                        let mono_out = {
                            let tab = &mut self.tabs[tab_idx];
                            for ch in tab.ch_samples.iter_mut() { let out = crate::wave::process_pitchshift_offline(&*ch, sr, sr, semi); *ch = out; }
                            let new_len = tab.ch_samples.get(0).map(|c| c.len()).unwrap_or(0); tab.samples_len = new_len; tab.dirty = true;
                            Self::editor_mixdown_mono(tab)
                        };
                        self.clear_preview_if_any(tab_idx);
                        self.audio.set_samples(std::sync::Arc::new(mono_out));
                        if let Some(tab) = self.tabs.get(tab_idx) { self.apply_loop_mode_for_tab(tab); }
                    }
                    if let Some(rate) = pending_stretch_apply {
                        let sr = self.audio.shared.out_sample_rate;
                        let mono_out = {
                            let tab = &mut self.tabs[tab_idx];
                            for ch in tab.ch_samples.iter_mut() { let out = crate::wave::process_timestretch_offline(&*ch, sr, sr, rate); *ch = out; }
                            let new_len = tab.ch_samples.get(0).map(|c| c.len()).unwrap_or(0); tab.samples_len = new_len; tab.dirty = true;
                            Self::editor_mixdown_mono(tab)
                        };
                        self.clear_preview_if_any(tab_idx);
                        self.audio.set_samples(std::sync::Arc::new(mono_out));
                        if let Some(tab) = self.tabs.get(tab_idx) { self.apply_loop_mode_for_tab(tab); }
                    }
                    if need_restore_preview { self.clear_preview_if_any(tab_idx); }
                    if let Some(s) = request_seek { self.audio.seek_to_sample(s); }
                    if let Some((tool_kind, mono)) = pending_preview { self.set_preview_mono(tab_idx, tool_kind, mono); }
                }); // end horizontal split

                // perform pending actions after borrows end
                // Defer starting heavy overlay until after UI to avoid nested &mut self borrow (E0499)
                if let Some((tool, p)) = pending_overlay_job { self.spawn_heavy_overlay_for_tab(tab_idx, tool, p); }
                if let Some((s,e)) = do_set_loop_from { if let Some(tab) = self.tabs.get_mut(tab_idx) { if s==0 && e==0 { tab.loop_region=None; } else { tab.loop_region=Some((s,e)); if tab.loop_mode==LoopMode::Marker { self.audio.set_loop_enabled(true); self.audio.set_loop_region(s,e); } } } }
                if let Some((s,e)) = do_trim { self.editor_apply_trim_range(tab_idx, (s,e)); }
                if let Some(((s,e), in_ms, out_ms)) = do_fade { self.editor_apply_fade_range(tab_idx, (s,e), in_ms, out_ms); }
                if let Some(((s,e), shp)) = do_fade_in { self.editor_apply_fade_in_explicit(tab_idx, (s,e), shp); }
                if let Some(((mut s,mut e), shp)) = do_fade_out {
                    // If range provided is (0, n) as length, anchor to end
                    if let Some(tab) = self.tabs.get(tab_idx) {
                        let len = tab.samples_len;
                        if s == 0 { s = len.saturating_sub(e); e = len; }
                    }
                    self.editor_apply_fade_out_explicit(tab_idx, (s,e), shp);
                }
                if let Some(((s,e), gdb)) = do_gain { self.editor_apply_gain_range(tab_idx, (s,e), gdb); }
                if let Some(((s,e), tdb)) = do_normalize { self.editor_apply_normalize_range(tab_idx, (s,e), tdb); }
                if let Some((s,e)) = do_reverse { self.editor_apply_reverse_range(tab_idx, (s,e)); }
                if let Some((_,_)) = do_cutjoin { if let Some(tab) = self.tabs.get_mut(tab_idx) { tab.trim_range = None; } }
                if let Some((s,e)) = do_cutjoin { self.editor_delete_range_and_join(tab_idx, (s,e)); }
                if do_apply_xfade { self.editor_apply_loop_xfade(tab_idx); }
                if apply_pending_loop { if let Some(tab_ro) = self.tabs.get(tab_idx) { self.apply_loop_mode_for_tab(tab_ro); } }
            } else {
                // List view
                // extracted implementation:
                {
                    self.ui_list_view(ui, ctx);
                }
                // legacy path kept under an always-false guard for transition
                if false {
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
                // moved to ui_list_view; do not draw here to avoid stray text
                // if self.files.is_empty() { ui.label("Select a folder to show list"); }
                }
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
            if let Some(idx) = self.active_tab { if let Some(tab) = self.tabs.get(idx) { self.apply_loop_mode_for_tab(tab); } }
            // Update effective volume to include per-file gain for the activated tab
            self.apply_effective_volume();
        }
        // Clear pending scroll flag after building the table
        self.scroll_to_selected = false;

        // Busy overlay (blocks input and shows loader)
        if self.processing.is_some() || self.export_state.is_some() || self.heavy_preview_rx.is_some() {
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
                        let msg = if let Some(p) = &self.processing { p.msg.as_str() }
                            else if let Some(st)=&self.export_state { st.msg.as_str() }
                            else if let Some(t)=&self.heavy_preview_tool { match t { ToolKind::PitchShift => "Previewing PitchShift...", ToolKind::TimeStretch => "Previewing TimeStretch...", _ => "Previewing..." } }
                            else { "Working..." };
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











