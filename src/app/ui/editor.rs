use std::path::PathBuf;

use crate::app::{helpers::*, types::*, LIVE_PREVIEW_SAMPLE_LIMIT};
use crate::wave::build_minmax;
use egui::*;

impl crate::app::WavesPreviewer {
    fn find_zero_cross_display(&self, tab_idx: usize, cur: usize, dir: i32) -> usize {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return cur;
        };
        let channel_count = tab.ch_samples.len();
        if channel_count == 0 {
            return cur;
        }
        let eps = self.zero_cross_epsilon.max(0.0);
        let mut visible = tab.channel_view.visible_indices(channel_count);
        let use_mixdown = tab.channel_view.mode == ChannelViewMode::Mixdown || visible.len() <= 1;
        let require_all = tab.channel_view.mode == ChannelViewMode::All;
        if require_all {
            visible = (0..channel_count).collect();
        }
        let min_len = tab
            .ch_samples
            .iter()
            .map(|c| c.len())
            .min()
            .unwrap_or(0);
        if min_len == 0 {
            return cur;
        }
        let cur = cur.min(min_len.saturating_sub(1));
        let is_cross = |prev: f32, cur: f32| -> bool {
            cur.abs() <= eps
                || prev.abs() <= eps
                || (prev > 0.0 && cur < 0.0)
                || (prev < 0.0 && cur > 0.0)
        };
        if use_mixdown {
            let mix_at = |idx: usize| -> f32 {
                let mut sum = 0.0f32;
                for ch in &tab.ch_samples {
                    if idx < ch.len() {
                        sum += ch[idx];
                    }
                }
                sum / channel_count as f32
            };
            if dir > 0 {
                if cur + 1 >= min_len {
                    return cur;
                }
                let mut prev = mix_at(cur);
                let mut i = cur + 1;
                while i < min_len {
                    let s = mix_at(i);
                    if is_cross(prev, s) {
                        return i;
                    }
                    prev = s;
                    i += 1;
                }
            } else if cur > 0 {
                let mut prev = mix_at(cur);
                let mut i = cur.saturating_sub(1);
                loop {
                    let s = mix_at(i);
                    if is_cross(prev, s) {
                        return i;
                    }
                    prev = s;
                    if i == 0 {
                        break;
                    }
                    i -= 1;
                }
            }
            return cur;
        }

        let mut prevs: Vec<f32> = Vec::with_capacity(visible.len());
        for &ch_idx in &visible {
            let ch = &tab.ch_samples[ch_idx];
            prevs.push(ch.get(cur).copied().unwrap_or(0.0));
        }
        if dir > 0 {
            if cur + 1 >= min_len {
                return cur;
            }
            let mut i = cur + 1;
            while i < min_len {
                let mut all_ok = true;
                for (slot, &ch_idx) in visible.iter().enumerate() {
                    let ch = &tab.ch_samples[ch_idx];
                    let s = ch.get(i).copied().unwrap_or(0.0);
                    if !is_cross(prevs[slot], s) {
                        all_ok = false;
                    }
                    prevs[slot] = s;
                }
                if all_ok {
                    return i;
                }
                i += 1;
            }
        } else if cur > 0 {
            let mut i = cur.saturating_sub(1);
            loop {
                let mut all_ok = true;
                for (slot, &ch_idx) in visible.iter().enumerate() {
                    let ch = &tab.ch_samples[ch_idx];
                    let s = ch.get(i).copied().unwrap_or(0.0);
                    if !is_cross(prevs[slot], s) {
                        all_ok = false;
                    }
                    prevs[slot] = s;
                }
                if all_ok {
                    return i;
                }
                if i == 0 {
                    break;
                }
                i -= 1;
            }
        }
        cur
    }

    pub(in crate::app) fn ui_editor_view(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        tab_idx: usize,
    ) {
        let mut apply_pending_loop = false;
        let mut do_commit_loop = false;
        let mut do_preview_unwrap: Option<u32> = None;
        let mut do_commit_markers = false;
        let mut pending_edit_undo: Option<EditorUndoState> = None;
        // Pre-read audio values to avoid borrowing self while editing tab
        let sr_ctx = self.audio.shared.out_sample_rate.max(1) as f32;
        let pos_audio_now = self
            .audio
            .shared
            .play_pos
            .load(std::sync::atomic::Ordering::Relaxed);
        let tab_samples_len = self.tabs[tab_idx].samples_len;
        let time_stretch_ratio = self.time_stretch_ratio_for_tab(&self.tabs[tab_idx]);
        let audio_len = self
            .audio
            .shared
            .samples
            .load()
            .as_ref()
            .map(|s| s.len())
            .unwrap_or(0);
        let map_audio_to_display = |audio_pos: usize| -> usize {
            let mapped = if let Some(ratio) = time_stretch_ratio {
                ((audio_pos as f32) / ratio).round() as usize
            } else {
                audio_pos
            };
            mapped.min(tab_samples_len)
        };
        let map_display_to_audio = |display_pos: usize| -> usize {
            if let Some(ratio) = time_stretch_ratio {
                let mapped = ((display_pos as f32) * ratio).round() as usize;
                if audio_len > 0 {
                    mapped.min(audio_len)
                } else {
                    mapped
                }
            } else {
                display_pos
            }
        };
        let playhead_display_now = map_audio_to_display(pos_audio_now);
        let mut request_seek: Option<usize> = None;
        let spec_path = self.tabs[tab_idx].path.clone();
        let spec_cache = self.spectro_cache.get(&spec_path).cloned();
        let spec_loading = self.spectro_inflight.contains(&spec_path);
        let mut touch_spectro_cache = false;
        ui.horizontal(|ui| {
            let tab = &self.tabs[tab_idx];
            let base = if self.is_virtual_path(&tab.path) {
                format!("{} (virtual)", tab.display_name)
            } else {
                tab.path.display().to_string()
            };
            ui.add(
                egui::Label::new(RichText::new(base).monospace())
                    .truncate()
                    .show_tooltip_when_elided(true),
            );
        });
        let mut discard_preview_for_view_change = false;
        let mut requested_channel_view: Option<ChannelView> = None;
        let channel_count = self.tabs[tab_idx].ch_samples.len();
        ui.horizontal_wrapped(|ui| {
            let tab = &mut self.tabs[tab_idx];
            // Loop mode toggles (kept): Off / OnWhole / Marker
            ui.label("Loop:");
            for (m, label) in [
                (LoopMode::Off, "Off"),
                (LoopMode::OnWhole, "On"),
                (LoopMode::Marker, "Marker"),
            ] {
                if ui.selectable_label(tab.loop_mode == m, label).clicked() {
                    tab.loop_mode = m;
                    apply_pending_loop = true;
                }
            }
            ui.separator();
            // View mode toggles
            let prev_view = tab.view_mode;
            for (vm, label) in [
                (ViewMode::Waveform, "Wave"),
                (ViewMode::Spectrogram, "Spec"),
                (ViewMode::Mel, "Mel"),
            ] {
                if ui.selectable_label(tab.view_mode == vm, label).clicked() {
                    tab.view_mode = vm;
                    if prev_view == ViewMode::Waveform && vm != ViewMode::Waveform {
                        tab.show_waveform_overlay = false;
                    }
                    if prev_view == ViewMode::Waveform && vm != ViewMode::Waveform {
                        discard_preview_for_view_change = true;
                    }
                }
            }
            ui.separator();
            // Channel view toggles
            if channel_count > 0 {
                let mut view = tab.channel_view.clone();
                let mut view_changed = false;
                ui.label("Ch:");
                if ui
                    .selectable_label(view.mode == ChannelViewMode::Mixdown, "Mix")
                    .clicked()
                {
                    view.mode = ChannelViewMode::Mixdown;
                    view_changed = true;
                }
                if ui
                    .selectable_label(view.mode == ChannelViewMode::All, "All")
                    .clicked()
                {
                    view.mode = ChannelViewMode::All;
                    view_changed = true;
                }
                ui.menu_button("Select", |ui| {
                    let mut selection_changed = false;
                    for idx in 0..channel_count {
                        let label = format!("Ch {}", idx + 1);
                        let mut selected = view.selected.contains(&idx);
                        if ui.checkbox(&mut selected, label).changed() {
                            selection_changed = true;
                            if selected {
                                if !view.selected.contains(&idx) {
                                    view.selected.push(idx);
                                }
                            } else {
                                view.selected.retain(|&v| v != idx);
                            }
                            }
                    }
                    if ui.button("Clear").clicked() {
                        view.selected.clear();
                        selection_changed = true;
                    }
                    if selection_changed {
                        view.mode = if view.selected.is_empty() {
                            ChannelViewMode::Mixdown
                        } else {
                            ChannelViewMode::Custom
                        };
                        view_changed = true;
                    }
                });
                if view_changed {
                    view.selected.retain(|&idx| idx < channel_count);
                    requested_channel_view = Some(view);
                }
            }
            ui.separator();
            let mut bpm_enabled = tab.bpm_enabled;
            if ui.checkbox(&mut bpm_enabled, "BPM").changed() {
                tab.bpm_enabled = bpm_enabled;
            }
            let mut bpm_value = tab.bpm_value;
            let bpm_resp = ui.add(
                egui::DragValue::new(&mut bpm_value)
                    .range(0.0..=300.0)
                    .speed(0.1)
                    .fixed_decimals(2)
                    .suffix(" BPM"),
            );
            if bpm_resp.changed() {
                tab.bpm_value = bpm_value.max(0.0);
                tab.bpm_user_set = true;
            }
            ui.separator();
            // Time HUD: play position (editable) / total length
            let sr = sr_ctx.max(1.0); // restore local sample-rate alias after removing top-level Loop block
            let mut pos_sec = playhead_display_now as f32 / sr;
            let mut len_sec = (tab.samples_len as f32 / sr).max(0.0);
            if !pos_sec.is_finite() {
                pos_sec = 0.0;
            }
            if !len_sec.is_finite() {
                len_sec = 0.0;
            }
            if pos_sec > len_sec {
                pos_sec = len_sec;
            }
            ui.label("Pos:");
            let pos_resp = ui.add(
                egui::DragValue::new(&mut pos_sec)
                    .range(0.0..=len_sec)
                    .speed(0.05)
                    .fixed_decimals(2),
            );
            if pos_resp.changed() {
                let display_samp = (pos_sec.max(0.0) * sr) as usize;
                let audio_samp = map_display_to_audio(display_samp);
                request_seek = Some(audio_samp);
            }
            ui.label(
                RichText::new(format!(
                    " / {}",
                    crate::app::helpers::format_time_s(len_sec)
                ))
                .monospace(),
            );
        });
        if let Some(view) = requested_channel_view.take() {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                tab.channel_view = view;
            }
            let path = self.tabs[tab_idx].path.clone();
            self.cancel_spectrogram_for_path(&path);
        }
        ui.separator();
        let _len_sec = if sr_ctx > 0.0 {
            (tab_samples_len as f32 / sr_ctx).max(0.0)
        } else {
            0.0
        };
        if !ctx.wants_keyboard_input() && tab_samples_len > 0 {
            let mods = ctx.input(|i| i.modifiers);
            let ctrl = mods.ctrl || mods.command;
            let alt = mods.alt;
            let pressed_left = ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft));
            let pressed_right = ctx.input(|i| i.key_pressed(egui::Key::ArrowRight));
            let left_down = ctx.input(|i| i.key_down(egui::Key::ArrowLeft));
            let right_down = ctx.input(|i| i.key_down(egui::Key::ArrowRight));
            let dir = if left_down ^ right_down {
                if right_down { 1 } else { -1 }
            } else {
                0
            };
            let mut hold = self.tabs[tab_idx].seek_hold.take();
            if dir != 0 {
                let now = std::time::Instant::now();
                let pressed = if dir > 0 { pressed_right } else { pressed_left };
                let repeat_delay = std::time::Duration::from_millis(220);
                let repeat_fast = std::time::Duration::from_millis(35);
                let repeat_slow = std::time::Duration::from_millis(70);
                let mut should_step = pressed;
                let mut hold_state = match hold.take() {
                    Some(mut state) => {
                        if state.dir != dir {
                            state = SeekHoldState {
                                dir,
                                started_at: now,
                                last_step_at: now,
                            };
                            should_step = true;
                        } else if !pressed {
                            let elapsed = now.saturating_duration_since(state.started_at);
                            let since = now.saturating_duration_since(state.last_step_at);
                            let interval = if elapsed >= std::time::Duration::from_millis(650) {
                                repeat_fast
                            } else {
                                repeat_slow
                            };
                            if elapsed >= repeat_delay && since >= interval {
                                should_step = true;
                            }
                        }
                        state
                    }
                    None => {
                        should_step = true;
                        SeekHoldState {
                            dir,
                            started_at: now,
                            last_step_at: now,
                        }
                    }
                };
                if should_step {
                    let cur_display = playhead_display_now;
                    let new_display = if alt {
                        self.find_zero_cross_display(tab_idx, cur_display, dir)
                    } else {
                        let spp = self.tabs[tab_idx].samples_per_px.max(0.0001);
                        let sr_u32 = self.audio.shared.out_sample_rate.max(1);
                        let sr = sr_u32 as f32;
                        let px_per_sec = (1.0 / spp) * sr;
                        let sample_step_sec = 1.0 / sr;
                        let sample_step = 1usize;
                        let time_grid_step = |min_px: f32| -> f32 {
                            let steps: [f32; 18] = [
                                0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0,
                                2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0,
                            ];
                            let mut step = steps[steps.len() - 1];
                            for s in steps {
                                if px_per_sec * s >= min_px {
                                    step = s;
                                    break;
                                }
                            }
                            step
                        };
                        let tab_bpm_enabled = self.tabs[tab_idx].bpm_enabled;
                        let bpm_value = self.tabs[tab_idx].bpm_value.max(1.0);
                        let use_bpm = tab_bpm_enabled && bpm_value >= 20.0;
                        let base_step_sec = if use_bpm {
                            let beat_sec = 60.0 / bpm_value;
                            let steps: [f32; 10] = [
                                1.0 / 64.0,
                                1.0 / 32.0,
                                1.0 / 16.0,
                                1.0 / 8.0,
                                1.0 / 4.0,
                                0.5,
                                1.0,
                                2.0,
                                4.0,
                                8.0,
                            ];
                            let px_per_beat = px_per_sec * beat_sec;
                            let mut step_beats = steps[steps.len() - 1];
                            for s in steps {
                                if px_per_beat * s >= 90.0 {
                                    step_beats = s;
                                    break;
                                }
                            }
                            (beat_sec * step_beats).max(sample_step_sec)
                        } else {
                            let mut base = time_grid_step(90.0);
                            if spp <= 1.0 {
                                base = sample_step_sec;
                            }
                            base.max(sample_step_sec)
                        };
                        let base_step_samples =
                            ((base_step_sec * sr).round() as usize).max(sample_step);
                        let fine_step_samples = (base_step_samples / 4).max(sample_step);
                        let step_samples = if ctrl {
                            base_step_samples
                        } else if mods.shift {
                            fine_step_samples
                        } else {
                            base_step_samples
                        };
                        let new_display = if ctrl {
                            if dir > 0 {
                                cur_display.saturating_add(step_samples)
                            } else {
                                cur_display.saturating_sub(step_samples)
                            }
                        } else if dir > 0 {
                            let target = cur_display.saturating_add(step_samples);
                            (target / step_samples) * step_samples
                        } else if cur_display == 0 {
                            0
                        } else {
                            let target = cur_display.saturating_sub(1);
                            (target / step_samples) * step_samples
                        };
                        new_display.min(tab_samples_len)
                    };
                    if new_display != cur_display {
                        request_seek = Some(map_display_to_audio(new_display));
                    }
                    hold_state.last_step_at = now;
                }
                hold = Some(hold_state);
            }
            self.tabs[tab_idx].seek_hold = hold;
        } else if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.seek_hold = None;
        }

        let avail = ui.available_size();
        // pending actions to perform after UI borrows end
        let mut do_set_loop_from: Option<(usize, usize)> = None;
        let mut do_trim: Option<(usize, usize)> = None; // keep-only (optional)
        let do_fade: Option<((usize, usize), f32, f32)> = None; // legacy whole-file fade
        let mut do_gain: Option<((usize, usize), f32)> = None;
        let mut do_normalize: Option<((usize, usize), f32)> = None;
        let mut do_reverse: Option<(usize, usize)> = None;
        // let mut do_silence: Option<(usize,usize)> = None; // removed
        let mut do_cutjoin: Option<(usize, usize)> = None;
        // Loop/marker apply handled via commit flags below.
        let mut do_fade_in: Option<((usize, usize), crate::app::types::FadeShape)> = None;
        let mut do_fade_out: Option<((usize, usize), crate::app::types::FadeShape)> = None;
        let mut stop_playback = false;
        // Snapshot busy state and prepare deferred overlay job.
        // IMPORTANT: Do NOT call `self.*` (which takes &mut self) while holding `let tab = &mut self.tabs[...]`.
        // That pattern triggers borrow checker error E0499. Defer such calls to after the UI closures.
        let overlay_busy = self.heavy_overlay_rx.is_some();
        let apply_busy = self.editor_apply_state.is_some();
        let mut pending_overlay_job: Option<(ToolKind, f32)> = None;
        let mut pending_overlay_path: Option<(ToolKind, PathBuf, f32)> = None;
        let mut request_undo = false;
        let mut request_redo = false;
        let gain_db = self
            .tabs
            .get(tab_idx)
            .map(|tab| self.pending_gain_db_for_path(&tab.path))
            .unwrap_or(0.0);
        let tab_path = self.tabs[tab_idx].path.clone();
        let apply_msg = self.editor_apply_state.as_ref().map(|s| s.msg.clone());
        let decode_status = self.editor_decode_state.as_ref().and_then(|state| {
            if state.path == tab_path {
                let (msg, progress) = if state.partial_ready {
                    ("Loading full audio".to_string(), 0.65f32)
                } else {
                    ("Decoding preview".to_string(), 0.25f32)
                };
                Some((msg, progress))
            } else {
                None
            }
        });
        let processing_msg = self
            .processing
            .as_ref()
            .filter(|p| p.path == tab_path)
            .map(|p| (p.msg.clone(), p.started_at));
        let preview_msg = if self.heavy_preview_rx.is_some() || self.heavy_overlay_rx.is_some() {
            let msg = if let Some(t) = &self.heavy_preview_tool {
                match t {
                    ToolKind::PitchShift => "Previewing PitchShift...".to_string(),
                    ToolKind::TimeStretch => "Previewing TimeStretch...".to_string(),
                    _ => "Previewing...".to_string(),
                }
            } else {
                "Previewing...".to_string()
            };
            Some(msg)
        } else {
            None
        };
        let spectro_loading = self.spectro_inflight.contains(&tab_path);
        let spectro_progress = self
            .spectro_progress
            .get(&tab_path)
            .map(|p| (p.done_tiles, p.total_tiles, p.started_at));
        let mut cancel_apply = false;
        let mut cancel_decode = false;
        let mut cancel_processing = false;
        let mut cancel_preview = false;
        let mut cancel_spectro = false;
        // Split canvas and inspector; keep inspector visible on narrow widths.
        let min_canvas_w = 160.0f32;
        let min_inspector_w = 220.0f32;
        let max_inspector_w = 360.0f32;
        let inspector_w = if avail.x <= min_inspector_w {
            avail.x
        } else {
            let available = (avail.x - min_canvas_w).max(min_inspector_w);
            available.min(max_inspector_w).min(avail.x)
        };
        let canvas_w = (avail.x - inspector_w).max(0.0);
        ui.horizontal(|ui| {
                let tab = &mut self.tabs[tab_idx];
                let preview_ok = tab.samples_len <= LIVE_PREVIEW_SAMPLE_LIMIT;
                // Canvas area
                let mut need_restore_preview = false;
                // Accumulate non-destructive preview audio to audition.
                // Carry the tool kind to keep preview state consistent.
                let mut pending_preview: Option<(ToolKind, Vec<f32>)> = None;
                let mut pending_heavy_preview: Option<(ToolKind, Vec<f32>, f32)> = None;
                let mut pending_heavy_preview_path: Option<(ToolKind, PathBuf, f32)> = None;
                let mut pending_pitch_apply: Option<f32> = None;
                let mut pending_stretch_apply: Option<f32> = None;
                let mut pending_loudness_apply: Option<f32> = None;
                if discard_preview_for_view_change {
                    need_restore_preview = true;
                    stop_playback = true;
                }
                ui.vertical(|ui| {
                    let canvas_h = (canvas_w * 0.35).clamp(180.0, avail.y);
                    let (resp, painter) = ui.allocate_painter(egui::vec2(canvas_w, canvas_h), Sense::click_and_drag());
                    let rect = resp.rect;
                    let w = rect.width().max(1.0); let h = rect.height().max(1.0);
                    let mut hover_cursor: Option<egui::CursorIcon> = None;
                    painter.rect_filled(rect, 0.0, Color32::from_rgb(16,16,18));
                    // Layout parameters
                    let gutter_w = 44.0;
                    let wave_left = rect.left() + gutter_w;
                    let wave_w = (w - gutter_w).max(1.0);
                        let channel_count = tab.ch_samples.len().max(1);
                        let mut visible_channels = tab.channel_view.visible_indices(channel_count);
                        let use_mixdown = tab.channel_view.mode == ChannelViewMode::Mixdown
                            || visible_channels.is_empty();
                        if use_mixdown {
                            visible_channels.clear();
                        }
                        let lane_count = if use_mixdown {
                            1
                        } else {
                            visible_channels.len().max(1)
                        };
                        let lane_h = h / lane_count as f32;

                    // Visual amplitude scale: assume Volume=0 dB for display; apply per-file Gain only
                    let scale = db_to_amp(gain_db);

                    // Initialize zoom to fit if unset (show whole file)
                    if tab.samples_len > 0 && tab.samples_per_px <= 0.0 {
                        let fit_spp = (tab.samples_len as f32 / wave_w.max(1.0)).max(0.01);
                        tab.samples_per_px = fit_spp;
                        tab.view_offset = 0;
                    }
                    // Keep the same center sample anchored when the window width changes.
                    if tab.samples_len > 0 {
                        let spp = tab.samples_per_px.max(0.0001);
                        let old_wave_w = tab.last_wave_w;
                        if old_wave_w > 0.0 && (old_wave_w - wave_w).abs() > 0.5 {
                            let old_vis = (old_wave_w * spp).ceil() as usize;
                            let new_vis = (wave_w * spp).ceil() as usize;
                            if old_vis > 0 && new_vis > 0 {
                                let anchor = tab.view_offset.saturating_add(old_vis / 2);
                                let max_left = tab.samples_len.saturating_sub(new_vis);
                                let new_view = anchor.saturating_sub(new_vis / 2).min(max_left);
                                tab.view_offset = new_view;
                            }
                        }
                        tab.last_wave_w = wave_w;
                    } else {
                        tab.last_wave_w = wave_w;
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
                    let fid = TextStyle::Monospace.resolve(ui.style());
                    let grid_col = Color32::from_rgb(38,38,44);
                    let label_col = Color32::GRAY;
                    if tab.bpm_enabled && tab.bpm_value >= 20.0 {
                        let bpm = tab.bpm_value.max(1.0);
                        let beat_sec = 60.0 / bpm;
                        let px_per_beat = px_per_sec * beat_sec;
                        let steps: [f32; 10] = [1.0/64.0, 1.0/32.0, 1.0/16.0, 1.0/8.0, 1.0/4.0, 0.5, 1.0, 2.0, 4.0, 8.0];
                        let mut step_beats = steps[steps.len() - 1];
                        for s in steps {
                            if px_per_beat * s >= min_px {
                                step_beats = s;
                                break;
                            }
                        }
                        let b0 = t0 / beat_sec;
                        let b1 = t1 / beat_sec;
                        let start_beat = (b0 / step_beats).floor() * step_beats;
                        let mut beat = start_beat;
                        let label_every = if step_beats < 0.25 {
                            1.0
                        } else if step_beats < 1.0 {
                            1.0
                        } else {
                            step_beats
                        };
                        while beat <= b1 + step_beats * 0.5 {
                            let t = beat * beat_sec;
                            let s_idx = (t * sr).round() as isize;
                            let rel = (s_idx.max(start as isize) - start as isize) as f32;
                            let x = wave_left + (rel / spp).clamp(0.0, wave_w);
                            painter.line_segment(
                                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                                egui::Stroke::new(1.0, grid_col),
                            );
                            if px_per_beat * step_beats >= 70.0
                                && ((beat / label_every).round() * label_every - beat).abs() < 1e-6
                            {
                                let label = if label_every >= 1.0 {
                                    format!("{:.0}b", beat)
                                } else {
                                    format!("{:.2}b", beat)
                                };
                                painter.text(
                                    egui::pos2(x + 2.0, rect.top() + 2.0),
                                    egui::Align2::LEFT_TOP,
                                    label,
                                    fid.clone(),
                                    label_col,
                                );
                            }
                            beat += step_beats;
                        }
                    } else {
                        let steps: [f32; 15] = [0.01,0.02,0.05,0.1,0.2,0.5,1.0,2.0,5.0,10.0,15.0,30.0,60.0,120.0,300.0];
                        let mut step = steps[steps.len()-1];
                        for s in steps { if px_per_sec * s >= min_px { step = s; break; } }
                        let start_tick = (t0 / step).floor() * step;
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
            }

            if tab.view_mode != ViewMode::Waveform {
                    if let Some(specs) = spec_cache.as_ref() {
                        touch_spectro_cache = true;
                        for ci in 0..lane_count {
                            let lane_top = rect.top() + lane_h * ci as f32;
                            let lane_rect = egui::Rect::from_min_size(
                                egui::pos2(wave_left, lane_top),
                                egui::vec2(wave_w, lane_h),
                            );
                            let spec = if use_mixdown {
                                specs.get(0)
                            } else if tab.channel_view.mode == ChannelViewMode::Custom {
                                specs.get(ci)
                            } else {
                                visible_channels
                                    .get(ci)
                                    .and_then(|idx| specs.get(*idx))
                                    .or_else(|| specs.get(ci))
                            };
                            if let Some(spec) = spec {
                                Self::draw_spectrogram(
                                    &painter,
                                    lane_rect,
                                    tab,
                                    spec,
                                    tab.view_mode,
                                    &self.spectro_cfg,
                                );
                            }
                        }
                    } else {
                    let fid = TextStyle::Monospace.resolve(ui.style());
                    let msg = if spec_loading { "Building spectrogram..." } else { "Spectrogram not ready" };
                    painter.text(
                        egui::pos2(wave_left + 6.0, rect.top() + 6.0),
                        egui::Align2::LEFT_TOP,
                        msg,
                        fid,
                        Color32::GRAY,
                    );
                }
                if !tab.show_waveform_overlay {
                    let sr = spec_cache
                        .as_ref()
                        .and_then(|specs| specs.get(0))
                        .map(|spec| spec.sample_rate)
                        .unwrap_or(self.audio.shared.out_sample_rate);
                    let mut max_freq = (sr.max(1) as f32) * 0.5;
                    if self.spectro_cfg.max_freq_hz > 0.0 {
                        max_freq = self.spectro_cfg.max_freq_hz.min(max_freq).max(1.0);
                    }
                    let log_min = 20.0_f32.min(max_freq).max(1.0);
                    let ticks_hz: [f32; 10] = [0.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0];
                    let fid = TextStyle::Monospace.resolve(ui.style());
                    let tick_col = Color32::from_rgb(140, 150, 165);
                    let tick_stroke = egui::Stroke::new(1.0, tick_col);
                    let freq_to_note_label = |freq: f32| -> String {
                        if freq <= 0.0 {
                            return String::new();
                        }
                        let note_f = 69.0 + 12.0 * (freq / 440.0).log2();
                        let note = note_f.round() as i32;
                        if note < 0 || note > 127 {
                            return String::new();
                        }
                        let names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
                        let idx = ((note % 12) + 12) % 12;
                        let octave = (note / 12) - 1;
                        format!("{}{}", names[idx as usize], octave)
                    };
                    let format_freq = |freq: f32| -> String {
                        if freq >= 1000.0 {
                            let k = freq / 1000.0;
                            if (k - k.round()).abs() < 0.05 {
                                format!("{:.0}k", k)
                            } else {
                                format!("{:.1}k", k)
                            }
                        } else {
                            format!("{:.0}", freq)
                        }
                    };
                    let mel_max = 2595.0 * (1.0 + max_freq / 700.0).log10();
                    let mel_min = 1.0_f32;
                    for ci in 0..lane_count {
                        let lane_top = rect.top() + lane_h * ci as f32;
                        let lane_rect = egui::Rect::from_min_size(
                            egui::pos2(wave_left, lane_top),
                            egui::vec2(wave_w, lane_h),
                        );
                        let mut last_y = f32::INFINITY;
                        for &freq in &ticks_hz {
                            if freq <= 0.0 || freq > max_freq {
                                if freq == 0.0 {
                                    // Keep 0 Hz label for context
                                } else {
                                    continue;
                                }
                            }
                            let frac = match tab.view_mode {
                                ViewMode::Spectrogram | ViewMode::Waveform => match self.spectro_cfg.scale {
                                    SpectrogramScale::Linear => (freq / max_freq).clamp(0.0, 1.0),
                                    SpectrogramScale::Log => {
                                        if freq <= 0.0 || max_freq <= log_min {
                                            0.0
                                        } else {
                                            let f = freq.clamp(log_min, max_freq);
                                            (f / log_min).ln() / (max_freq / log_min).ln()
                                        }
                                    }
                                },
                                ViewMode::Mel => match self.spectro_cfg.mel_scale {
                                    SpectrogramScale::Linear => {
                                        let mel = 2595.0 * (1.0 + (freq / 700.0)).log10();
                                        (mel / mel_max).clamp(0.0, 1.0)
                                    }
                                    SpectrogramScale::Log => {
                                        let mel = 2595.0 * (1.0 + (freq / 700.0)).log10();
                                        if mel_max <= mel_min {
                                            (mel / mel_max.max(1.0)).clamp(0.0, 1.0)
                                        } else {
                                            (mel / mel_min)
                                                .ln()
                                                .clamp(0.0, (mel_max / mel_min).ln())
                                                / (mel_max / mel_min).ln()
                                        }
                                    }
                                },
                            };
                            let y = lane_rect.bottom() - frac * lane_rect.height();
                            if last_y.is_finite() && (last_y - y) < 12.0 {
                                continue;
                            }
                            let label = if self.spectro_cfg.show_note_labels {
                                let note = freq_to_note_label(freq);
                                if note.is_empty() {
                                    format_freq(freq)
                                } else {
                                    format!("{} {}", format_freq(freq), note)
                                }
                            } else {
                                format_freq(freq)
                            };
                            painter.line_segment(
                                [egui::pos2(wave_left - 6.0, y), egui::pos2(wave_left - 2.0, y)],
                                tick_stroke,
                            );
                            painter.text(
                                egui::pos2(rect.left() + 2.0, y),
                                egui::Align2::LEFT_CENTER,
                                label,
                                fid.clone(),
                                tick_col,
                            );
                            last_y = y;
                        }
                    }
                }
            }

            // Handle interactions (seek, zoom, pan, selection)
            if tab.view_mode == ViewMode::Waveform && tab.samples_len > 0 && !ctx.wants_keyboard_input() {
                let zoom_in = ctx.input(|i| i.key_pressed(egui::Key::ArrowUp));
                let zoom_out = ctx.input(|i| i.key_pressed(egui::Key::ArrowDown));
                if zoom_in || zoom_out {
                    let factor = if zoom_in { 0.9 } else { 1.1 };
                    let old_spp = tab.samples_per_px.max(0.0001);
                    let vis = (wave_w * old_spp).ceil() as usize;
                    let playhead = playhead_display_now.min(tab.samples_len);
                    let anchor = if playhead >= tab.view_offset
                        && playhead <= tab.view_offset.saturating_add(vis)
                    {
                        playhead
                    } else {
                        tab.view_offset.saturating_add(vis / 2)
                    };
                    let t = if vis > 0 {
                        ((anchor.saturating_sub(tab.view_offset)) as f32 / vis as f32)
                            .clamp(0.0, 1.0)
                    } else {
                        0.5
                    };
                    let min_spp = 0.01;
                    let max_spp_fit = (tab.samples_len as f32 / wave_w.max(1.0)).max(min_spp);
                    let new_spp = (old_spp * factor).clamp(min_spp, max_spp_fit);
                    tab.samples_per_px = new_spp;
                    let vis2 = (wave_w * tab.samples_per_px).ceil() as usize;
                    let left = anchor.saturating_sub((t * vis2 as f32) as usize);
                    let max_left = tab.samples_len.saturating_sub(vis2);
                    tab.view_offset = left.min(max_left);
                }
            }

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
                        egui::Event::MouseWheel { delta, .. } => {
                            wheel += delta;
                        }
                        egui::Event::Zoom(z) => {
                            pinch_zoom_factor *= z;
                        }
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
            // Drag markers for LoopEdit / Trim (primary button only)
            let mut suppress_seek = false;
            if pointer_over_canvas
                && matches!(tab.active_tool, ToolKind::LoopEdit | ToolKind::Trim)
                && tab.samples_len > 0
            {
                let pointer_down = ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary));
                let pointer_released = ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary));
                if pointer_released || !pointer_down {
                    tab.dragging_marker = None;
                }
                if pointer_down {
                    let spp = tab.samples_per_px.max(0.0001);
                    let vis = (wave_w * spp).ceil() as usize;
                    let to_sample = |x: f32| {
                        let x = x.clamp(wave_left, wave_left + wave_w);
                        let pos = (((x - wave_left) / wave_w) * vis as f32) as usize;
                        tab.view_offset.saturating_add(pos).min(tab.samples_len)
                    };
                    let to_x = |samp: usize| {
                        wave_left
                            + (((samp.saturating_sub(tab.view_offset)) as f32 / spp)
                                .clamp(0.0, wave_w))
                    };
                    let hit_radius = 7.0;
                    if tab.dragging_marker.is_none() {
                        if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                            let x = pos.x;
                            match tab.active_tool {
                                ToolKind::LoopEdit => {
                                    if let Some((a0, b0)) = tab.loop_region {
                                        let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                        let ax = to_x(a);
                                        let bx = to_x(b);
                                        if (x - ax).abs() <= hit_radius {
                                            if pending_edit_undo.is_none() {
                                                pending_edit_undo = Some(Self::capture_undo_state(tab));
                                            }
                                            tab.dragging_marker = Some(MarkerKind::A);
                                        } else if (x - bx).abs() <= hit_radius {
                                            if pending_edit_undo.is_none() {
                                                pending_edit_undo = Some(Self::capture_undo_state(tab));
                                            }
                                            tab.dragging_marker = Some(MarkerKind::B);
                                        }
                                    }
                                }
                                                                    ToolKind::Trim => {
                                    if let Some((a0, b0)) = tab.trim_range {
                                        let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                        let ax = to_x(a);
                                        let bx = to_x(b);
                                        if (x - ax).abs() <= hit_radius {
                                            if pending_edit_undo.is_none() {
                                                pending_edit_undo = Some(Self::capture_undo_state(tab));
                                            }
                                            tab.dragging_marker = Some(MarkerKind::A);
                                        } else if (x - bx).abs() <= hit_radius {
                                            if pending_edit_undo.is_none() {
                                                pending_edit_undo = Some(Self::capture_undo_state(tab));
                                            }
                                            tab.dragging_marker = Some(MarkerKind::B);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    if let Some(marker) = tab.dragging_marker {
                        if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                            let samp = to_sample(pos.x);
                            match tab.active_tool {
                                ToolKind::LoopEdit => {
                                    if let Some((a0, b0)) = tab.loop_region {
                                        let (mut a, mut b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                        if pending_edit_undo.is_none() {
                                            pending_edit_undo = Some(Self::capture_undo_state(tab));
                                        }
                                        match marker {
                                            MarkerKind::A => a = samp.min(b),
                                            MarkerKind::B => b = samp.max(a),
                                        }
                                        tab.loop_region = Some((a, b));
                                        Self::update_loop_markers_dirty(tab);
                                        apply_pending_loop = true;
                                    }
                                }
                                ToolKind::Trim => {
                                    if let Some((a0, b0)) = tab.trim_range {
                                        let (mut a, mut b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                        if pending_edit_undo.is_none() {
                                            pending_edit_undo = Some(Self::capture_undo_state(tab));
                                        }
                                        match marker {
                                            MarkerKind::A => a = samp.min(b),
                                            MarkerKind::B => b = samp.max(a),
                                        }
                                        tab.trim_range = Some((a, b));
                                    }
                                }
                                _ => {}
                            }
                        }
                        suppress_seek = true;
                    }
                }
            }
            // Drag to select a range (independent of tool), unless we are dragging markers
            let alt_now = ui.input(|i| i.modifiers.alt);
            if pointer_over_canvas
                && !alt_now
                && tab.samples_len > 0
                && tab.dragging_marker.is_none()
            {
                let drag_started = resp.drag_started_by(egui::PointerButton::Primary);
                let dragging = resp.dragged_by(egui::PointerButton::Primary);
                let drag_released = resp.drag_stopped_by(egui::PointerButton::Primary);
                if drag_started {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let spp = tab.samples_per_px.max(0.0001);
                        let vis = (wave_w * spp).ceil() as usize;
                        let x = pos.x.clamp(wave_left, wave_left + wave_w);
                        let samp = tab
                            .view_offset
                            .saturating_add((((x - wave_left) / wave_w) * vis as f32) as usize)
                            .min(tab.samples_len);
                        tab.drag_select_anchor = Some(samp);
                    }
                }
                if dragging {
                    let anchor = tab.drag_select_anchor.or_else(|| {
                        resp.interact_pointer_pos().map(|pos| {
                            let spp = tab.samples_per_px.max(0.0001);
                            let vis = (wave_w * spp).ceil() as usize;
                            let x = pos.x.clamp(wave_left, wave_left + wave_w);
                            tab.view_offset
                                .saturating_add(
                                    (((x - wave_left) / wave_w) * vis as f32) as usize,
                                )
                                .min(tab.samples_len)
                        })
                    });
                    if tab.drag_select_anchor.is_none() {
                        tab.drag_select_anchor = anchor;
                    }
                    if let (Some(anchor), Some(pos)) = (anchor, resp.interact_pointer_pos()) {
                        let spp = tab.samples_per_px.max(0.0001);
                        let vis = (wave_w * spp).ceil() as usize;
                        let x = pos.x.clamp(wave_left, wave_left + wave_w);
                        let samp = tab
                            .view_offset
                            .saturating_add((((x - wave_left) / wave_w) * vis as f32) as usize)
                            .min(tab.samples_len);
                        let (s, e) = if samp >= anchor {
                            (anchor, samp)
                        } else {
                            (samp, anchor)
                        };
                        tab.selection = Some((s, e));
                        if tab.active_tool == ToolKind::Trim {
                            tab.trim_range = Some((s, e));
                        }
                        suppress_seek = true;
                    }
                }
                if drag_released {
                    tab.drag_select_anchor = None;
                }
            }
            // Selection vs Seek with primary button (Alt+LeftDrag = pan handled above)
            if !alt_now && !suppress_seek {
                // Primary interactions: click to seek (no range selection)
                if resp.clicked_by(egui::PointerButton::Primary) {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let x = pos.x.clamp(wave_left, wave_left + wave_w);
                        let spp = tab.samples_per_px.max(0.0001);
                        let vis = (wave_w * spp).ceil() as usize;
                        let pos_samp = tab
                            .view_offset
                            .saturating_add(
                                (((x - wave_left) / wave_w) * vis as f32) as usize,
                            )
                            .min(tab.samples_len);
                        if let Some((a0, b0)) = tab.selection {
                            let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                            if pos_samp < a || pos_samp > b {
                                tab.selection = None;
                                if tab.active_tool == ToolKind::Trim {
                                    tab.trim_range = None;
                                }
                            }
                        }
                        request_seek = Some(map_display_to_audio(pos_samp));
                    }
                }
            }

            let spp = tab.samples_per_px.max(0.0001);
            let vis = (wave_w * spp).ceil() as usize;
            let start = tab.view_offset.min(tab.samples_len);
            let end = (start + vis).min(tab.samples_len);
            let visible_len = end.saturating_sub(start);

            let mixdown_visible = if use_mixdown && visible_len > 0 && !tab.ch_samples.is_empty() {
                let mut out = vec![0.0f32; visible_len];
                let chn = tab.ch_samples.len() as f32;
                if chn > 0.0 {
                    for ch in &tab.ch_samples {
                        for (i, v) in ch[start..end].iter().enumerate() {
                            if let Some(dst) = out.get_mut(i) {
                                *dst += *v;
                            }
                        }
                    }
                    for v in &mut out {
                        *v /= chn;
                    }
                }
                out
            } else {
                Vec::new()
            };

            let show_waveform = tab.view_mode == ViewMode::Waveform || tab.show_waveform_overlay;

            // Draw per-channel lanes with dB grid and playhead
            if show_waveform {
            for lane_idx in 0..lane_count {
                let channel_index = if use_mixdown {
                    None
                } else {
                    visible_channels.get(lane_idx).copied()
                };
                let lane_top = rect.top() + lane_h * lane_idx as f32;
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

                if visible_len > 0 {
                    // Two rendering paths depending on zoom level:
                    // - Aggregated min/max bins for spp >= 1.0 (>= 1 sample per pixel)
                    // - Direct per-sample polyline/stem for spp < 1.0 (< 1 sample per pixel)
                    if spp >= 1.0 {
                        let bins = wave_w as usize; // one bin per pixel
                        if bins > 0 {
                            let mut tmp = Vec::new();
                            if use_mixdown {
                                build_minmax(&mut tmp, &mixdown_visible, bins);
                            } else if let Some(ch) = channel_index.and_then(|idx| tab.ch_samples.get(idx)) {
                                build_minmax(&mut tmp, &ch[start..end], bins);
                            }
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
                        if tab.active_tool != ToolKind::Trim && tab.preview_overlay.is_some() {
                            if let Some(overlay) = &tab.preview_overlay {
                                let och: Option<&[f32]> = if use_mixdown {
                                    overlay
                                        .mixdown
                                        .as_ref()
                                        .map(|v| v.as_slice())
                                        .or_else(|| overlay.channels.get(0).map(|v| v.as_slice()))
                                } else {
                                    channel_index
                                        .and_then(|idx| overlay.channels.get(idx).map(|v| v.as_slice()))
                                        .or_else(|| overlay.channels.get(0).map(|v| v.as_slice()))
                                };
                                if let Some(buf) = och {
                                    use crate::app::render::overlay as ov;
                                    use crate::app::render::colors::{OVERLAY_COLOR, OVERLAY_STROKE_BASE, OVERLAY_STROKE_EMPH};
                                    let base_total = tab.samples_len.max(1);
                                    let overlay_total = overlay.timeline_len.max(1);
                                    let is_time_stretch = matches!(overlay.source_tool, ToolKind::TimeStretch);
                                    let unwrap_preview = matches!(overlay.source_tool, ToolKind::LoopEdit)
                                        && overlay_total > base_total
                                        && tab.pending_loop_unwrap.is_some()
                                        && tab.loop_region.is_some();
                                    let ratio = if is_time_stretch {
                                        1.0
                                    } else if base_total == 0 {
                                        1.0
                                    } else {
                                        overlay_total as f32 / base_total as f32
                                    };
                                    let start_scaled = ((start as f32) * ratio).round() as usize;
                                    let mut vis_scaled = ((visible_len as f32) * ratio).ceil() as usize;
                                    if vis_scaled == 0 { vis_scaled = 1; }
                                    let (startb, _endb, over_vis) = ov::map_visible_overlay(start_scaled, vis_scaled, overlay_total, buf.len());
                                    if over_vis > 0 {
                                        let bins = wave_w as usize;
                                        let bins_values = if unwrap_preview {
                                            let loop_start = tab.loop_region.map(|(a, _)| a).unwrap_or(0);
                                            ov::compute_overlay_bins_for_unwrap(
                                                start,
                                                visible_len,
                                                base_total,
                                                loop_start,
                                                buf,
                                                overlay_total,
                                                bins,
                                            )
                                        } else {
                                            ov::compute_overlay_bins_for_base_columns(start, visible_len, startb, over_vis, buf, bins)
                                        };
                                        // Draw full overlay
                                        ov::draw_bins_locked(&painter, lane_rect, wave_w, &bins_values, scale, OVERLAY_COLOR, OVERLAY_STROKE_BASE);
                                        // Emphasize LoopEdit boundary segments if applicable
                                        if tab.active_tool == ToolKind::LoopEdit && !unwrap_preview {
                                            if let Some((a, b)) = tab.loop_region {
                                                let cf = Self::effective_loop_xfade_samples(
                                                    a,
                                                    b,
                                                    tab.samples_len,
                                                    tab.loop_xfade_samples,
                                                );
                                                if cf > 0 {
                                                    // Map required pre/post segments into overlay domain using ratio
                                                    let ratio = if base_total > 0 { (overlay_total as f32) / (base_total as f32) } else { 1.0 };
                                                    let pre0 = a.saturating_sub(cf);
                                                    let pre1 = (a + cf).min(tab.samples_len);
                                                    let post0 = b.saturating_sub(cf);
                                                    let post1 = (b + cf).min(tab.samples_len);
                                                    let a0 = (((pre0 as f32) * ratio).round() as usize).min(buf.len());
                                                    let a1 = (((pre1 as f32) * ratio).round() as usize).min(buf.len());
                                                    let b0 = (((post0 as f32) * ratio).round() as usize).min(buf.len());
                                                    let b1 = (((post1 as f32) * ratio).round() as usize).min(buf.len());
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
                let samples = if use_mixdown {
                    mixdown_visible.as_slice()
                } else {
                    channel_index
                        .and_then(|idx| tab.ch_samples.get(idx))
                        .map(|ch| &ch[start..end])
                        .unwrap_or(&[])
                };
                if visible_len == 1 {
                    let sx = lane_rect.left() + wave_w * 0.5;
                    let v = samples
                        .get(0)
                        .copied()
                        .unwrap_or(0.0)
                        .mul_add(scale, 0.0)
                        .clamp(-1.0, 1.0);
                    let sy = lane_rect.center().y - v * scale_y;
                    let col = amp_to_color(v.abs().clamp(0.0, 1.0));
                    painter.circle_filled(egui::pos2(sx, sy), 2.0, col);
                } else {
                    let denom = (visible_len - 1) as f32;
                    let mut last: Option<(f32, f32, egui::Color32)> = None;
                    for (i, &v0) in samples.iter().enumerate() {
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
                        for (i, &v0) in samples.iter().enumerate() {
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
                if self.debug.cfg.enabled && self.debug.overlay_trace {
                    let mode = if spp >= 1.0 { "agg" } else { "line" };
                    let has_ov = tab.preview_overlay.is_some();
                    eprintln!(
                        "OVERLAY gate: mode={} has_overlay={} active={:?} spp={:.5} vis_len={} start={} end={} view_off={} len={}",
                        mode, has_ov, tab.active_tool, spp, visible_len, start, end, tab.view_offset, tab.samples_len
                    );
                }
                if tab.active_tool != ToolKind::Trim && tab.preview_overlay.is_some() {
                    if let Some(overlay) = &tab.preview_overlay {
                        // try channel match, fallback to first channel if overlay is mono
                        let och: Option<&[f32]> = if use_mixdown {
                            overlay
                                .mixdown
                                .as_ref()
                                .map(|v| v.as_slice())
                                .or_else(|| overlay.channels.get(0).map(|v| v.as_slice()))
                        } else {
                            channel_index
                                .and_then(|idx| overlay.channels.get(idx).map(|v| v.as_slice()))
                                .or_else(|| overlay.channels.get(0).map(|v| v.as_slice()))
                        };
                        if let Some(buf) = och {
                            use crate::app::render::overlay as ov;
                            let base_total = tab.samples_len.max(1);
                            let overlay_total = overlay.timeline_len.max(1);
                            let unwrap_preview = matches!(overlay.source_tool, ToolKind::LoopEdit)
                                && overlay_total > base_total
                                && tab.pending_loop_unwrap.is_some()
                                && tab.loop_region.is_some();
                            if unwrap_preview {
                                if let Some((loop_start, _)) = tab.loop_region {
                                    let bins = wave_w as usize;
                                    if bins > 0 {
                                        let values = ov::compute_overlay_bins_for_unwrap(
                                            start,
                                            visible_len.max(1),
                                            base_total,
                                            loop_start,
                                            buf,
                                            overlay_total,
                                            bins,
                                        );
                                        ov::draw_bins_locked(
                                            &painter,
                                            lane_rect,
                                            wave_w,
                                            &values,
                                            scale,
                                            egui::Color32::from_rgb(80, 240, 160),
                                            1.3,
                                        );
                                    }
                                }
                            } else {
                                // Map original-visible [start,end) to overlay domain using length ratio.
                                // This keeps overlays visible at any zoom, even when length differs (e.g. TimeStretch).
                                let lenb = buf.len();
                                let is_time_stretch = matches!(overlay.source_tool, ToolKind::TimeStretch);
                                let ratio = if is_time_stretch {
                                    1.0
                                } else if base_total > 0 {
                                    (overlay_total as f32) / (base_total as f32)
                                } else {
                                    1.0
                                };
                            let orig_vis = visible_len.max(1);
                            // Map visible window [start .. start+orig_vis) into overlay domain using total-length ratio
                            // Align overlay start to original start using nearest sample to minimize off-by-one drift
                            let startb = (((start as f32) * ratio).round() as usize).min(lenb);
                            let mut endb = startb + (((orig_vis as f32) * ratio).ceil() as usize);
                            if endb > lenb { endb = lenb; }
                            if startb >= endb { endb = (startb + 1).min(lenb); }
                            let over_vis = (endb.saturating_sub(startb)).max(1);
                            let r_w = if orig_vis > 0 { (over_vis as f32) / (orig_vis as f32) } else { 1.0 };
                            let ov_w = (wave_w * r_w).max(1.0);
                            #[cfg(debug_assertions)]
                            if self.debug.cfg.enabled && self.debug.overlay_trace {
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
                                        let cf = Self::effective_loop_xfade_samples(
                                            a,
                                            b,
                                            tab.samples_len,
                                            tab.loop_xfade_samples,
                                        );
                                        if cf > 0 {
                                            let pre0 = a.saturating_sub(cf);
                                            let pre1 = (a + cf).min(tab.samples_len);
                                            let post0 = b.saturating_sub(cf);
                                            let post1 = (b + cf).min(tab.samples_len);
                                            let a0 = (((pre0 as f32) * ratio).round() as usize).min(lenb);
                                            let a1 = (((pre1 as f32) * ratio).round() as usize).min(lenb);
                                            let b0 = (((post0 as f32) * ratio).round() as usize).min(lenb);
                                            let b1 = (((post1 as f32) * ratio).round() as usize).min(lenb);
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
                                    if self.debug.cfg.enabled && self.debug.overlay_trace {
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
                                    if self.debug.cfg.enabled && self.debug.overlay_trace {
                                        eprintln!("OVERLAY seg: fallback_tick used at x={:.1}", sx);
                                    }
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
                                                let s = start.min(lenb);
                                                let e = end.min(lenb);
                                                if s < e {
                                                    build_minmax(&mut tmp, &buf[s..e], bins);
                                                }
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
                }
            }

            // (Removed) global mono overlay to avoid double/triple drawing.

            // Overlay regions (loop/trim/fade) on top of waveform
            if tab.samples_len > 0 {
                let to_x = |samp: usize| {
                    wave_left
                        + (((samp.saturating_sub(tab.view_offset)) as f32 / spp)
                            .clamp(0.0, wave_w))
                };
                let draw_handle = |x: f32, col: Color32| {
                    let handle_w = 6.0;
                    let handle_h = 16.0;
                    let r = egui::Rect::from_min_max(
                        egui::pos2(x - handle_w * 0.5, rect.top()),
                        egui::pos2(x + handle_w * 0.5, rect.top() + handle_h),
                    );
                    painter.rect_filled(r, 2.0, col);
                };
                let sr = self.audio.shared.out_sample_rate.max(1) as f32;

                let mut fade_in_handle: Option<f32> = None;
                let mut fade_out_handle: Option<f32> = None;

                // Selection overlay (Trim tool only)
                if tab.active_tool == ToolKind::Trim {
                    if let Some((a0, b0)) = tab.selection {
                        let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                        if b >= tab.view_offset {
                            let vis = (wave_w * spp).ceil() as usize;
                            let end = tab.view_offset.saturating_add(vis).min(tab.samples_len);
                            if a <= end {
                                let ax = to_x(a);
                                let bx = to_x(b);
                                let sel_rect = egui::Rect::from_min_max(
                                    egui::pos2(ax, rect.top()),
                                    egui::pos2(bx, rect.bottom()),
                                );
                                let fill = Color32::from_rgba_unmultiplied(70, 140, 255, 28);
                                let stroke = Color32::from_rgba_unmultiplied(70, 140, 255, 140);
                                painter.rect_filled(sel_rect, 0.0, fill);
                                painter.rect_stroke(
                                    sel_rect,
                                    0.0,
                                    egui::Stroke::new(1.0, stroke),
                                    egui::StrokeKind::Inside,
                                );
                            }
                        }
                    }
                }

                // Marker overlay
                if !tab.markers.is_empty() {
                    let vis = (wave_w * spp).ceil() as usize;
                    let start = tab.view_offset.min(tab.samples_len);
                    let end = (start + vis).min(tab.samples_len);
                    let pending = tab.markers != tab.markers_committed;
                    let col = if pending {
                        Color32::from_rgb(120, 220, 120)
                    } else {
                        Color32::from_rgb(255, 200, 80)
                    };
                    for m in tab.markers.iter() {
                        if m.sample < start || m.sample > end {
                            continue;
                        }
                        let x = to_x(m.sample);
                        painter.line_segment(
                            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                            egui::Stroke::new(1.0, col),
                        );
                    }
                }

                // Loop overlay
                if let Some((a0, b0)) = tab.loop_region {
                    let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                    let active = tab.active_tool == ToolKind::LoopEdit;
                    let line_alpha = if active { 220 } else { 160 };
                    let line = Color32::from_rgba_unmultiplied(60, 160, 255, line_alpha);
                    let fid = TextStyle::Monospace.resolve(ui.style());
                    let ax = to_x(a);
                    if b == a {
                        painter.line_segment(
                            [egui::pos2(ax, rect.top()), egui::pos2(ax, rect.bottom())],
                            egui::Stroke::new(2.0, line),
                        );
                        draw_handle(ax, line);
                        painter.text(
                            egui::pos2(ax + 6.0, rect.top() + 2.0),
                            egui::Align2::LEFT_TOP,
                            "S",
                            fid,
                            Color32::from_rgb(170, 200, 255),
                        );
                    } else {
                        let bx = to_x(b);
                        let shade_alpha = if active { 40 } else { 22 };
                        let shade = Color32::from_rgba_unmultiplied(60, 160, 255, shade_alpha);
                        let r = egui::Rect::from_min_max(
                            egui::pos2(ax, rect.top()),
                            egui::pos2(bx, rect.bottom()),
                        );
                        painter.rect_filled(r, 0.0, shade);
                        painter.line_segment(
                            [egui::pos2(ax, rect.top()), egui::pos2(ax, rect.bottom())],
                            egui::Stroke::new(2.0, line),
                        );
                        painter.line_segment(
                            [egui::pos2(bx, rect.top()), egui::pos2(bx, rect.bottom())],
                            egui::Stroke::new(2.0, line),
                        );
                        draw_handle(ax, line);
                        draw_handle(bx, line);
                        painter.text(
                            egui::pos2(ax + 6.0, rect.top() + 2.0),
                            egui::Align2::LEFT_TOP,
                            "S",
                            fid.clone(),
                            Color32::from_rgb(170, 200, 255),
                        );
                        painter.text(
                            egui::pos2(bx + 6.0, rect.top() + 2.0),
                            egui::Align2::LEFT_TOP,
                            "E",
                            fid.clone(),
                            Color32::from_rgb(170, 200, 255),
                        );
                        let dur = (b.saturating_sub(a)) as f32 / sr;
                        let label = crate::app::helpers::format_time_s(dur);
                        painter.text(
                            egui::pos2(ax + 6.0, rect.top() + 18.0),
                            egui::Align2::LEFT_TOP,
                            format!("Loop {label}"),
                            fid,
                            Color32::from_rgb(160, 190, 230),
                        );

                        // Crossfade bands and shape
                        let cf = Self::effective_loop_xfade_samples(
                            a,
                            b,
                            tab.samples_len,
                            tab.loop_xfade_samples,
                        );
                        if cf > 0 {
                            let pre0 = a.saturating_sub(cf);
                            let pre1 = (a + cf).min(tab.samples_len);
                            let post0 = b.saturating_sub(cf);
                            let post1 = (b + cf).min(tab.samples_len);
                            let xs0 = to_x(pre0);
                            let xs1 = to_x(pre1);
                            let xe0 = to_x(post0);
                            let xe1 = to_x(post1);
                            let band_alpha = if active { 50 } else { 28 };
                            let col_in = Color32::from_rgba_unmultiplied(255, 180, 60, band_alpha);
                            let col_out = Color32::from_rgba_unmultiplied(60, 180, 255, band_alpha);
                            let r_in = egui::Rect::from_min_max(
                                egui::pos2(xs0, rect.top()),
                                egui::pos2(xs1, rect.bottom()),
                            );
                            let r_out = egui::Rect::from_min_max(
                                egui::pos2(xe0, rect.top()),
                                egui::pos2(xe1, rect.bottom()),
                            );
                            painter.rect_filled(r_in, 0.0, col_in);
                            painter.rect_filled(r_out, 0.0, col_out);

                            let curve_alpha = if active { 220 } else { 140 };
                            let curve_col = Color32::from_rgba_unmultiplied(255, 170, 60, curve_alpha);
                            let steps = 36;
                            let mut last_in_up: Option<egui::Pos2> = None;
                            let mut last_in_down: Option<egui::Pos2> = None;
                            let mut last_out_up: Option<egui::Pos2> = None;
                            let mut last_out_down: Option<egui::Pos2> = None;
                            let h = rect.height();
                            let y_of = |w: f32| rect.bottom() - w * h;
                            for i in 0..=steps {
                                let t = (i as f32) / (steps as f32);
                                let (w_out, w_in) = match tab.loop_xfade_shape {
                                    crate::app::types::LoopXfadeShape::EqualPower => {
                                        let a = core::f32::consts::FRAC_PI_2 * t;
                                        (a.cos(), a.sin())
                                    }
                                    crate::app::types::LoopXfadeShape::Linear => (1.0 - t, t),
                                };
                                let x_in = egui::lerp(xs0..=xs1, t);
                                let p_in_up = egui::pos2(x_in, y_of(w_in));
                                let p_in_down = egui::pos2(x_in, y_of(w_out));
                                if let Some(lp) = last_in_up {
                                    painter.line_segment(
                                        [lp, p_in_up],
                                        egui::Stroke::new(2.0, curve_col),
                                    );
                                }
                                if let Some(lp) = last_in_down {
                                    painter.line_segment(
                                        [lp, p_in_down],
                                        egui::Stroke::new(2.0, curve_col),
                                    );
                                }
                                last_in_up = Some(p_in_up);
                                last_in_down = Some(p_in_down);

                                let x_out = egui::lerp(xe0..=xe1, t);
                                let p_out_up = egui::pos2(x_out, y_of(w_in));
                                let p_out_down = egui::pos2(x_out, y_of(w_out));
                                if let Some(lp) = last_out_up {
                                    painter.line_segment(
                                        [lp, p_out_up],
                                        egui::Stroke::new(2.0, curve_col),
                                    );
                                }
                                if let Some(lp) = last_out_down {
                                    painter.line_segment(
                                        [lp, p_out_down],
                                        egui::Stroke::new(2.0, curve_col),
                                    );
                                }
                                last_out_up = Some(p_out_up);
                                last_out_down = Some(p_out_down);
                            }
                        }
                    }
                }

                // Trim overlay
                if tab.active_tool == ToolKind::Trim {
                    if let Some((a0, b0)) = tab.trim_range {
                        let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                        let line = Color32::from_rgba_unmultiplied(255, 140, 0, 230);
                        let fid = TextStyle::Monospace.resolve(ui.style());
                        let ax = to_x(a);
                        if b == a {
                            painter.line_segment(
                                [egui::pos2(ax, rect.top()), egui::pos2(ax, rect.bottom())],
                                egui::Stroke::new(2.0, line),
                            );
                            draw_handle(ax, line);
                            painter.text(
                                egui::pos2(ax + 6.0, rect.top() + 2.0),
                                egui::Align2::LEFT_TOP,
                                "A",
                                fid,
                                Color32::from_rgb(255, 200, 150),
                            );
                        } else {
                            let bx = to_x(b);
                            let dim = Color32::from_rgba_unmultiplied(0, 0, 0, 80);
                            let keep = Color32::from_rgba_unmultiplied(255, 160, 60, 36);
                            let left = egui::Rect::from_min_max(
                                egui::pos2(rect.left(), rect.top()),
                                egui::pos2(ax, rect.bottom()),
                            );
                            let right = egui::Rect::from_min_max(
                                egui::pos2(bx, rect.top()),
                                egui::pos2(rect.right(), rect.bottom()),
                            );
                            painter.rect_filled(left, 0.0, dim);
                            painter.rect_filled(right, 0.0, dim);
                            let keep_r = egui::Rect::from_min_max(
                                egui::pos2(ax, rect.top()),
                                egui::pos2(bx, rect.bottom()),
                            );
                            painter.rect_filled(keep_r, 0.0, keep);
                            painter.line_segment(
                                [egui::pos2(ax, rect.top()), egui::pos2(ax, rect.bottom())],
                                egui::Stroke::new(2.0, line),
                            );
                            painter.line_segment(
                                [egui::pos2(bx, rect.top()), egui::pos2(bx, rect.bottom())],
                                egui::Stroke::new(2.0, line),
                            );
                            draw_handle(ax, line);
                            draw_handle(bx, line);
                            painter.text(
                                egui::pos2(ax + 6.0, rect.top() + 2.0),
                                egui::Align2::LEFT_TOP,
                                "A",
                                fid.clone(),
                                Color32::from_rgb(255, 200, 150),
                            );
                            painter.text(
                                egui::pos2(bx + 6.0, rect.top() + 2.0),
                                egui::Align2::LEFT_TOP,
                                "B",
                                fid,
                                Color32::from_rgb(255, 200, 150),
                            );
                        }
                    }
                }

                // Fade overlays
                let draw_fade = |x0: f32, x1: f32, shape: crate::app::types::FadeShape, is_in: bool, base_col: Color32| {
                    let steps = 28;
                    let max_alpha = 80.0;
                    for i in 0..steps {
                        let t0 = i as f32 / steps as f32;
                        let t1 = (i + 1) as f32 / steps as f32;
                        let w0 = if is_in { Self::fade_weight(shape, t0) } else { Self::fade_weight_out(shape, t0) };
                        let w1 = if is_in { Self::fade_weight(shape, t1) } else { Self::fade_weight_out(shape, t1) };
                        let vol0 = w0;
                        let vol1 = w1;
                        let vol = (vol0 + vol1) * 0.5;
                        let alpha = ((1.0 - vol) * max_alpha).clamp(0.0, 255.0) as u8;
                        if alpha == 0 { continue; }
                        let rx0 = egui::lerp(x0..=x1, t0);
                        let rx1 = egui::lerp(x0..=x1, t1);
                        let r = egui::Rect::from_min_max(
                            egui::pos2(rx0, rect.top()),
                            egui::pos2(rx1, rect.bottom()),
                        );
                        painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(base_col.r(), base_col.g(), base_col.b(), alpha));
                    }
                    let curve_col = Color32::from_rgba_unmultiplied(base_col.r(), base_col.g(), base_col.b(), 200);
                    let mut last: Option<egui::Pos2> = None;
                    for i in 0..=steps {
                        let t = i as f32 / steps as f32;
                        let w = if is_in { Self::fade_weight(shape, t) } else { Self::fade_weight_out(shape, t) };
                        let vol = w;
                        let x = egui::lerp(x0..=x1, t);
                        let y = rect.bottom() - vol * rect.height();
                        let p = egui::pos2(x, y);
                        if let Some(lp) = last {
                            painter.line_segment([lp, p], egui::Stroke::new(2.0, curve_col));
                        }
                        last = Some(p);
                    }
                };
                if tab.active_tool == ToolKind::Fade {
                    let n_in = ((tab.tool_state.fade_in_ms / 1000.0) * sr).round() as usize;
                    if n_in > 0 {
                        let end = n_in.min(tab.samples_len);
                        let x0 = to_x(0);
                        let x1 = to_x(end);
                        if x1 > x0 + 1.0 {
                            draw_fade(
                                x0,
                                x1,
                                tab.fade_in_shape,
                                true,
                                Color32::from_rgb(80, 180, 255),
                            );
                            fade_in_handle = Some(x1);
                            let fid = TextStyle::Monospace.resolve(ui.style());
                            let secs = (end as f32) / sr;
                            painter.text(
                                egui::pos2(x0 + 6.0, rect.bottom() - 18.0),
                                egui::Align2::LEFT_BOTTOM,
                                format!(
                                    "Fade In {}",
                                    crate::app::helpers::format_time_s(secs)
                                ),
                                fid,
                                Color32::from_rgb(150, 190, 230),
                            );
                        }
                    }
                    let n_out = ((tab.tool_state.fade_out_ms / 1000.0) * sr).round() as usize;
                    if n_out > 0 {
                        let start_out = tab.samples_len.saturating_sub(n_out);
                        let x0 = to_x(start_out);
                        let x1 = to_x(tab.samples_len);
                        if x1 > x0 + 1.0 {
                            draw_fade(
                                x0,
                                x1,
                                tab.fade_out_shape,
                                false,
                                Color32::from_rgb(255, 160, 90),
                            );
                            fade_out_handle = Some(x0);
                            let fid = TextStyle::Monospace.resolve(ui.style());
                            let secs = (n_out as f32) / sr;
                            painter.text(
                                egui::pos2(x0 + 6.0, rect.bottom() - 18.0),
                                egui::Align2::LEFT_BOTTOM,
                                format!(
                                    "Fade Out {}",
                                    crate::app::helpers::format_time_s(secs)
                                ),
                                fid,
                                Color32::from_rgb(230, 190, 150),
                            );
                        }
                    }
                }

                // Cursor feedback for editor handles
                if pointer_over_canvas {
                    let handle_radius = 7.0;
                    if tab.dragging_marker.is_some() {
                        hover_cursor = Some(egui::CursorIcon::ResizeHorizontal);
                    } else if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                        let x = pos.x;
                        let near = |hx: f32| (x - hx).abs() <= handle_radius;
                        match tab.active_tool {
                            ToolKind::LoopEdit => {
                                if let Some((a0, b0)) = tab.loop_region {
                                    let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                    let ax = to_x(a);
                                    let bx = to_x(b);
                                    if near(ax) || near(bx) {
                                        hover_cursor = Some(egui::CursorIcon::ResizeHorizontal);
                                    }
                                }
                            }
                            ToolKind::Trim => {
                                if let Some((a0, b0)) = tab.trim_range {
                                    let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                    let ax = to_x(a);
                                    let bx = to_x(b);
                                    if near(ax) || near(bx) {
                                        hover_cursor = Some(egui::CursorIcon::ResizeHorizontal);
                                    }
                                }
                            }
                            ToolKind::Fade => {
                                if let Some(xh) = fade_in_handle {
                                    if near(xh) {
                                        hover_cursor = Some(egui::CursorIcon::ResizeHorizontal);
                                    }
                                }
                                if let Some(xh) = fade_out_handle {
                                    if near(xh) {
                                        hover_cursor = Some(egui::CursorIcon::ResizeHorizontal);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if let Some(icon) = hover_cursor {
                    ui.output_mut(|o| o.cursor_icon = icon);
                }
            }

            // Shared playhead across lanes
            if tab.samples_len > 0 {
                if let Some(buf) = self.audio.shared.samples.load().as_ref() {
                    let len = buf.len().max(1);
                    let pos_audio = self
                        .audio
                        .shared
                        .play_pos
                        .load(std::sync::atomic::Ordering::Relaxed)
                        .min(len);
                    let pos = map_audio_to_display(pos_audio);
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

            // Horizontal scrollbar when zoomed in
            if tab.samples_len > 0 {
                let spp = tab.samples_per_px.max(0.0001);
                let vis = (wave_w * spp).ceil() as usize;
                let max_left = tab.samples_len.saturating_sub(vis);
                if tab.view_offset > max_left {
                    tab.view_offset = max_left;
                }
                if max_left > 0 {
                    let mut off = tab.view_offset as f32;
                    let resp = ui.add(
                        egui::Slider::new(&mut off, 0.0..=max_left as f32)
                            .show_value(false)
                            .clamping(egui::SliderClamping::Always),
                    );
                    if resp.changed() {
                        tab.view_offset = off.round().clamp(0.0, max_left as f32) as usize;
                    }
                }
            }

            if tab.loading {
                let (msg, progress) = decode_status
                    .as_ref()
                    .map(|(m, p)| (m.as_str(), *p))
                    .unwrap_or(("Loading audio", 0.0));
                let overlay_rect = egui::Rect::from_min_size(
                    egui::pos2(wave_left, rect.top()),
                    egui::vec2(wave_w, rect.height()),
                )
                .shrink(10.0);
                painter.rect_filled(
                    overlay_rect,
                    6.0,
                    Color32::from_rgba_unmultiplied(0, 0, 0, 150),
                );
                let fid = TextStyle::Monospace.resolve(ui.style());
                let label = if tab.samples_len == 0 {
                    format!("{msg}...")
                } else {
                    format!("Preview ready. {msg}...")
                };
                painter.text(
                    overlay_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    label,
                    fid,
                    Color32::from_rgb(220, 220, 230),
                );
                let bar_w = overlay_rect.width().min(240.0);
                let bar_h = 6.0;
                let bar_left = overlay_rect.center().x - (bar_w * 0.5);
                let bar_top = overlay_rect.center().y + 18.0;
                let bar_rect = egui::Rect::from_min_size(
                    egui::pos2(bar_left, bar_top),
                    egui::vec2(bar_w, bar_h),
                );
                painter.rect_filled(bar_rect, 3.0, Color32::from_rgb(40, 40, 45));
                let fill_w = (bar_w * progress.clamp(0.0, 1.0)).max(2.0);
                let fill_rect = egui::Rect::from_min_size(
                    egui::pos2(bar_left, bar_top),
                    egui::vec2(fill_w, bar_h),
                );
                painter.rect_filled(fill_rect, 3.0, Color32::from_rgb(90, 160, 240));
            }
                }); // end canvas UI

                // Inspector area (right)
                ui.vertical(|ui| {
                    ui.set_width(inspector_w);
                    ui.heading("Inspector");
                    ui.separator();
                    if let Some((msg, progress)) = decode_status.as_ref() {
                        ui.horizontal_wrapped(|ui| {
                            ui.add(egui::Spinner::new());
                            ui.label(RichText::new(msg.as_str()).strong());
                            ui.add(
                                egui::ProgressBar::new(*progress)
                                    .desired_width(120.0)
                                    .show_percentage(),
                            );
                            if ui.button("Cancel").clicked() {
                                cancel_decode = true;
                            }
                        });
                        ui.separator();
                    }
                    if let Some(apply_msg) = apply_msg.as_ref() {
                        ui.horizontal_wrapped(|ui| {
                            ui.add(egui::Spinner::new());
                            ui.label(RichText::new(apply_msg.as_str()).strong());
                            if ui.button("Cancel").clicked() {
                                cancel_apply = true;
                            }
                        });
                        ui.separator();
                    }
                    if let Some((msg, started_at)) = processing_msg {
                        let elapsed = started_at.elapsed().as_secs_f32();
                        ui.horizontal_wrapped(|ui| {
                            ui.add(egui::Spinner::new());
                            ui.label(RichText::new(format!(
                                "{} ({:.1}s)",
                                msg,
                                elapsed
                            )).weak());
                            if ui.button("Cancel").clicked() {
                                cancel_processing = true;
                            }
                        });
                        ui.separator();
                    }
                    if let Some(msg) = preview_msg.as_ref() {
                        ui.horizontal_wrapped(|ui| {
                            ui.add(egui::Spinner::new());
                            ui.label(RichText::new(msg.as_str()).weak());
                            if ui.button("Cancel").clicked() {
                                cancel_preview = true;
                            }
                        });
                        ui.separator();
                    }
                    if spectro_loading {
                        let (done, total, started_at) = spectro_progress.unwrap_or((0, 0, std::time::Instant::now()));
                        let pct = if total > 0 {
                            (done as f32 / total as f32).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        let elapsed = started_at.elapsed().as_secs_f32();
                        ui.horizontal_wrapped(|ui| {
                            ui.add(egui::Spinner::new());
                            ui.label(RichText::new(format!(
                                "Spectrogram... ({:.1}s)",
                                elapsed
                            )).weak());
                            if total > 0 {
                                ui.add(
                                    egui::ProgressBar::new(pct)
                                        .desired_width(120.0)
                                        .show_percentage(),
                                );
                            }
                            if ui.button("Cancel").clicked() {
                                cancel_spectro = true;
                            }
                        });
                        ui.separator();
                    }
                    let can_undo = !tab.undo_stack.is_empty();
                    let can_redo = !tab.redo_stack.is_empty();
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(can_undo, egui::Button::new("Undo"))
                            .clicked()
                        {
                            request_undo = true;
                        }
                        if ui
                            .add_enabled(can_redo, egui::Button::new("Redo"))
                            .clicked()
                        {
                            request_redo = true;
                        }
                    });
                    ui.separator();
                    match tab.view_mode {
                        ViewMode::Waveform => {
                            // Tool selector
                            let mut tool = tab.active_tool;
                            egui::ComboBox::new("tool_selector", "Tool")
                                .selected_text(format!("{:?}", tool))
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut tool, ToolKind::LoopEdit, "Loop Edit");
                                    ui.selectable_value(&mut tool, ToolKind::Markers, "Markers");
                                    ui.selectable_value(&mut tool, ToolKind::Trim, "Trim");
                                    ui.selectable_value(&mut tool, ToolKind::Fade, "Fade");
                                    ui.selectable_value(&mut tool, ToolKind::PitchShift, "PitchShift");
                                    ui.selectable_value(&mut tool, ToolKind::TimeStretch, "TimeStretch");
                                    ui.selectable_value(&mut tool, ToolKind::Gain, "Gain");
                                    ui.selectable_value(&mut tool, ToolKind::Normalize, "Normalize");
                                    ui.selectable_value(&mut tool, ToolKind::Loudness, "LoudNorm");
                                    ui.selectable_value(&mut tool, ToolKind::Reverse, "Reverse");
                                });
                            if tool != tab.active_tool {
                                tab.active_tool_last = Some(tab.active_tool);
                                // Leaving Markers/LoopEdit: discard un-applied preview markers/loops
                                if matches!(tab.active_tool, ToolKind::Markers) {
                                    if tab.markers != tab.markers_committed {
                                        tab.markers = tab.markers_committed.clone();
                                        tab.markers_dirty = tab.markers_committed != tab.markers_saved;
                                    }
                                }
                                if matches!(tab.active_tool, ToolKind::LoopEdit) {
                                    if tab.loop_region != tab.loop_region_committed {
                                        tab.loop_region = tab.loop_region_committed;
                                    }
                                    tab.pending_loop_unwrap = None;
                                    if tab.markers != tab.markers_committed {
                                        tab.markers = tab.markers_committed.clone();
                                        tab.markers_dirty = tab.markers_committed != tab.markers_saved;
                                    }
                                    Self::update_loop_markers_dirty(tab);
                                }
                                // Leaving a tool: discard any preview overlay/audio
                                if tab.preview_audio_tool.is_some() || tab.preview_overlay.is_some() {
                                    need_restore_preview = true;
                                }
                                stop_playback = true;
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
                                            let resp_s = ui.add(egui::DragValue::new(&mut s_i).range(0..=max_i).speed(64.0));
                                            ui.label("End:");
                                            let resp_e = ui.add(egui::DragValue::new(&mut e_i).range(0..=max_i).speed(64.0));
                                            if (resp_s.gained_focus() || resp_s.drag_started()
                                                || resp_e.gained_focus()
                                                || resp_e.drag_started())
                                                && pending_edit_undo.is_none()
                                            {
                                                pending_edit_undo = Some(Self::capture_undo_state(tab));
                                            }
                                            let chs = resp_s.changed();
                                            let che = resp_e.changed();
                                            if chs || che {
                                                let mut s = s_i.clamp(0, max_i) as usize;
                                                let mut e = e_i.clamp(0, max_i) as usize;
                                                if e < s { std::mem::swap(&mut s, &mut e); }
                                                tab.loop_region = Some((s,e));
                                                tab.pending_loop_unwrap = None;
                                                tab.preview_audio_tool = None;
                                                tab.preview_overlay = None;
                                                Self::update_loop_markers_dirty(tab);
                                                apply_pending_loop = true;
                                            }
                                        });
                                        // Crossfade controls (duration in ms + shape)
                                        let sr = self.audio.shared.out_sample_rate.max(1) as f32;
                                        let mut x_ms = (tab.loop_xfade_samples as f32 / sr) * 1000.0;
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label("Xfade (ms):");
                                            let resp_x = ui.add(egui::DragValue::new(&mut x_ms).range(0.0..=5000.0).speed(5.0).fixed_decimals(1));
                                            if (resp_x.gained_focus() || resp_x.drag_started()) && pending_edit_undo.is_none() {
                                                pending_edit_undo = Some(Self::capture_undo_state(tab));
                                            }
                                            if resp_x.changed() {
                                                let samp = ((x_ms / 1000.0) * sr).round().clamp(0.0, tab.samples_len as f32) as usize;
                                                tab.loop_xfade_samples = samp;
                                                apply_pending_loop = true;
                                            }
                                            ui.label("Shape:");
                                            let mut shp = tab.loop_xfade_shape;
                                            egui::ComboBox::from_id_salt("xfade_shape").selected_text(match shp { crate::app::types::LoopXfadeShape::Linear => "Linear", crate::app::types::LoopXfadeShape::EqualPower => "Equal" }).show_ui(ui, |ui| {
                                                ui.selectable_value(&mut shp, crate::app::types::LoopXfadeShape::Linear, "Linear");
                                                ui.selectable_value(&mut shp, crate::app::types::LoopXfadeShape::EqualPower, "Equal");
                                            });
                                            if shp != tab.loop_xfade_shape {
                                                if pending_edit_undo.is_none() {
                                                    pending_edit_undo = Some(Self::capture_undo_state(tab));
                                                }
                                                tab.loop_xfade_shape = shp;
                                                apply_pending_loop = true;
                                            }
                                        });
                                        ui.horizontal_wrapped(|ui| {
                                            if ui.button("Set Start").on_hover_text("Set Start at playhead").clicked() {
                                                if pending_edit_undo.is_none() {
                                                    pending_edit_undo = Some(Self::capture_undo_state(tab));
                                                }
                                                let pos = playhead_display_now;
                                                let end = tab.loop_region.map(|(_,e)| e).unwrap_or(pos);
                                                let (mut s, mut e) = (pos, end);
                                                if e < s { std::mem::swap(&mut s, &mut e); }
                                                tab.loop_region = Some((s,e));
                                                tab.pending_loop_unwrap = None;
                                                tab.preview_audio_tool = None;
                                                tab.preview_overlay = None;
                                                Self::update_loop_markers_dirty(tab);
                                                apply_pending_loop = true;
                                            }
                                            if ui.button("Set End").on_hover_text("Set End at playhead").clicked() {
                                                if pending_edit_undo.is_none() {
                                                    pending_edit_undo = Some(Self::capture_undo_state(tab));
                                                }
                                                let pos = playhead_display_now;
                                                let start = tab.loop_region.map(|(s,_)| s).unwrap_or(pos);
                                                let (mut s, mut e) = (start, pos);
                                                if e < s { std::mem::swap(&mut s, &mut e); }
                                                tab.loop_region = Some((s,e));
                                                tab.pending_loop_unwrap = None;
                                                tab.preview_audio_tool = None;
                                                tab.preview_overlay = None;
                                                Self::update_loop_markers_dirty(tab);
                                                apply_pending_loop = true;
                                            }
                                            if ui.button("Clear").clicked() {
                                                if pending_edit_undo.is_none() {
                                                    pending_edit_undo = Some(Self::capture_undo_state(tab));
                                                }
                                                do_set_loop_from = Some((0,0));
                                            }
                                        });

                                        // Crossfade controls already above; add Apply button to destructively bake Xfade
                                        ui.horizontal_wrapped(|ui| {
                                            let mut repeat = tab.tool_state.loop_repeat.max(2);
                                            ui.label("Repeat:");
                                            if ui
                                                .add(
                                                    egui::DragValue::new(&mut repeat)
                                                        .range(2..=128)
                                                        .speed(1),
                                                )
                                                .changed()
                                            {
                                                tab.tool_state =
                                                    ToolState { loop_repeat: repeat, ..tab.tool_state };
                                            }
                                            let has_loop = tab
                                                .loop_region
                                                .map(|(a, b)| b > a)
                                                .unwrap_or(false);
                                            if ui
                                                .add_enabled(
                                                    has_loop && !apply_busy,
                                                    egui::Button::new(format!("Unwrap x{}", repeat)),
                                                )
                                                .on_hover_text("Preview loop unwrap (non-destructive until Apply)")
                                                .clicked()
                                            {
                                                if pending_edit_undo.is_none() {
                                                    pending_edit_undo = Some(Self::capture_undo_state(tab));
                                                }
                                                do_preview_unwrap = Some(repeat);
                                                stop_playback = true;
                                                tab.pending_loop_unwrap = Some(repeat);
                                                tab.preview_audio_tool = None;
                                                tab.preview_overlay = None;
                                            }                                        ui.horizontal_wrapped(|ui| {
                                            let effective_cf = tab
                                                .loop_region
                                                .map(|(a, b)| {
                                                    Self::effective_loop_xfade_samples(
                                                        a,
                                                        b,
                                                        tab.samples_len,
                                                        tab.loop_xfade_samples,
                                                    )
                                                })
                                                .unwrap_or(0);
                                            let is_loop_dirty = tab.loop_region != tab.loop_region_committed;
                                            let unwrap_pending = tab.pending_loop_unwrap.is_some();
                                            let can_apply = (is_loop_dirty || effective_cf > 0 || unwrap_pending) && !apply_busy;
                                            if ui
                                                .add_enabled(
                                                    can_apply,
                                                    egui::Button::new("Apply"),
                                                )
                                                .on_hover_text(
                                                    "Commit loop changes and bake crossfade",
                                                )
                                                .clicked()
                                            {
                                                do_commit_loop = true;
                                            }
                                        });


                                        });

                                        // Dynamic preview overlay for LoopEdit (non-destructive):
                                        // Build a mono preview applying the current loop crossfade to the mixdown.
                                        if !preview_ok {
                                            ui.label(RichText::new("Preview disabled for large clips").weak());
                                        } else if let Some((a,b)) = tab.loop_region {
                                            let cf = Self::effective_loop_xfade_samples(
                                                a,
                                                b,
                                                tab.samples_len,
                                                tab.loop_xfade_samples,
                                            );
                                            if cf > 0 {
                                                // Build per-channel overlay applying crossfade across centered windows
                                                let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                let win_len = cf.saturating_mul(2);
                                                let denom = (win_len.saturating_sub(1)).max(1) as f32;
                                                let s_start = a.saturating_sub(cf);
                                                let e_start = b.saturating_sub(cf);
                                                for ch in overlay.iter_mut() {
                                                    for i in 0..win_len {
                                                        let s_idx = s_start.saturating_add(i);
                                                        let e_idx = e_start.saturating_add(i);
                                                        if s_idx >= ch.len() || e_idx >= ch.len() {
                                                            break;
                                                        }
                                                        let t = (i as f32) / denom;
                                                        let (w_out, w_in) = match tab.loop_xfade_shape {
                                                            crate::app::types::LoopXfadeShape::EqualPower => {
                                                                let ang = core::f32::consts::FRAC_PI_2 * t; (ang.cos(), ang.sin())
                                                            }
                                                            crate::app::types::LoopXfadeShape::Linear => (1.0 - t, t),
                                                        };
                                                        let s = ch[s_idx];
                                                        let e = ch[e_idx];
                                                        let mixed = e * w_out + s * w_in;
                                                        ch[s_idx] = mixed;
                                                        ch[e_idx] = mixed;
                                                    }
                                                }
                                                let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(tab.samples_len);
                                                tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                                                    overlay,
                                                    ToolKind::LoopEdit,
                                                    timeline_len,
                                                ));
                                                tab.preview_audio_tool = Some(ToolKind::LoopEdit);
                                            }
                                        }
                                    });
                                }

                                                                    ToolKind::Markers => {
                                    ui.scope(|ui| {
                                        let s = ui.style_mut();
                                        s.spacing.item_spacing = egui::vec2(6.0, 6.0);
                                        s.spacing.button_padding = egui::vec2(6.0, 3.0);
                                        let out_sr = self.audio.shared.out_sample_rate.max(1) as f32;
                                        ui.horizontal_wrapped(|ui| {
                                            if ui.button("Add at Playhead").clicked() {
                                                if pending_edit_undo.is_none() {
                                                    pending_edit_undo = Some(Self::capture_undo_state(tab));
                                                }
                                                let pos = playhead_display_now;
                                                let label = Self::next_marker_label(&tab.markers);
                                                let entry = crate::markers::MarkerEntry {
                                                    sample: pos,
                                                    label,
                                                };
                                                match tab.markers.binary_search_by_key(&pos, |m| m.sample) {
                                                    Ok(idx) => {
                                                        tab.markers[idx] = entry;
                                                    }
                                                    Err(idx) => {
                                                        tab.markers.insert(idx, entry);
                                                    }
                                                }
                                            }
                                            if ui
                                                .add_enabled(
                                                    !tab.markers.is_empty(),
                                                    egui::Button::new("Clear"),
                                                )
                                                .clicked()
                                            {
                                                if pending_edit_undo.is_none() {
                                                    pending_edit_undo = Some(Self::capture_undo_state(tab));
                                                }
                                                tab.markers.clear();
                                            }
                                        });
                                        ui.horizontal_wrapped(|ui| {
                                            let can_apply = tab.markers != tab.markers_committed && !apply_busy;
                                            if ui
                                                .add_enabled(can_apply, egui::Button::new("Apply"))
                                                .on_hover_text("Commit markers (written on Save Selected)")
                                                .clicked()
                                            {
                                                do_commit_markers = true;
                                            }
                                        });
                                        ui.label(format!("Count: {}", tab.markers.len()));
                                        if !tab.markers.is_empty() {
                                            let (dot_color, dot_hint) = if tab.markers == tab.markers_saved {
                                                (
                                                    Color32::from_rgb(160, 200, 160),
                                                    "Saved (written to file)",
                                                )
                                            } else if tab.markers == tab.markers_committed {
                                                (
                                                    Color32::from_rgb(255, 180, 60),
                                                    "Applied (pending save)",
                                                )
                                            } else {
                                                (
                                                    Color32::from_rgb(120, 220, 120),
                                                    "Preview (not applied)",
                                                )
                                            };
                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    RichText::new("\u{25CF}")
                                                        .color(dot_color)
                                                        .strong(),
                                                );
                                                ui.label("Label");
                                                ui.label("Sec");
                                                ui.label("Time");
                                                ui.label("");
                                            });
                                            let samples_len = tab.samples_len;
                                            let mut len_sec = (samples_len as f32 / out_sr).max(0.0);
                                            if !len_sec.is_finite() { len_sec = 0.0; }
                                            let mut markers_local = tab.markers.clone();
                                            let mut remove_idx: Option<usize> = None;
                                            let mut resort = false;
                                            let mut dirty = false;
                                            egui::ScrollArea::vertical()
                                                .max_height(160.0)
                                                .show(ui, |ui| {
                                                    for (idx, m) in markers_local.iter_mut().enumerate() {
                                                        let mut secs = (m.sample as f32) / out_sr;
                                                        if !secs.is_finite() { secs = 0.0; }
                                                        if secs > len_sec { secs = len_sec; }
                                                        ui.horizontal(|ui| {
                                                            ui.label(
                                                                RichText::new("\u{25CF}")
                                                                    .color(dot_color),
                                                            )
                                                            .on_hover_text(dot_hint);
                                                            let resp = ui.add(
                                                                egui::TextEdit::singleline(&mut m.label)
                                                                    .desired_width(80.0),
                                                            );
                                                            if resp.changed() {
                                                                dirty = true;
                                                            }
                                                            let resp_time = ui.add(
                                                                egui::DragValue::new(&mut secs)
                                                                    .range(0.0..=len_sec)
                                                                    .speed(0.01)
                                                                    .fixed_decimals(3),
                                                            );
                                                            let time_changed = resp_time.changed();
                                                            if time_changed {
                                                                let sample = ((secs.max(0.0)) * out_sr)
                                                                    .round() as usize;
                                                                m.sample = sample.min(samples_len);
                                                                dirty = true;
                                                                resort = true;
                                                            }
                                                            ui.label(crate::app::helpers::format_time_s(secs));
                                                            if ui.button("Delete").clicked() {
                                                                remove_idx = Some(idx);
                                                            }
                                                        });
                                                    }
                                                });
                                            if let Some(idx) = remove_idx {
                                                if idx < markers_local.len() {
                                                    markers_local.remove(idx);
                                                }
                                                dirty = true;
                                            }
                                            if resort {
                                                markers_local.sort_by_key(|m| m.sample);
                                            }
                                            if dirty {
                                                if pending_edit_undo.is_none() {
                                                    pending_edit_undo = Some(Self::capture_undo_state(tab));
                                                }
                                                tab.markers = markers_local;
                                            }
                                        }
                                    });
                                }
    ToolKind::Trim => {
                                    ui.scope(|ui| {
                                        let s = ui.style_mut();
                                        s.spacing.item_spacing = egui::vec2(6.0, 6.0);
                                        s.spacing.button_padding = egui::vec2(6.0, 3.0);
                                        if !preview_ok {
                                            ui.label(RichText::new("Preview disabled for large clips").weak());
                                        }
                                        // Trim has its own A/B range (independent from loop)
                                        let mut range_opt = tab.trim_range;
                                        let sr = self.audio.shared.out_sample_rate.max(1) as f32;
                                        let mut len_sec = (tab.samples_len as f32 / sr).max(0.0);
                                        if !len_sec.is_finite() { len_sec = 0.0; }
                                        if let Some((smp,emp)) = range_opt { ui.label(format!("Trim A?B: {}..{} samp", smp, emp)); } else { ui.label("Trim A?B: (set below)"); }
                                        ui.horizontal_wrapped(|ui| {
                                            let mut s_sec = range_opt.map(|(s, _)| s as f32 / sr).unwrap_or(0.0);
                                            let mut e_sec = range_opt.map(|(_, e)| e as f32 / sr).unwrap_or(0.0);
                                            if !s_sec.is_finite() { s_sec = 0.0; }
                                            if !e_sec.is_finite() { e_sec = 0.0; }
                                            if s_sec > len_sec { s_sec = len_sec; }
                                            if e_sec > len_sec { e_sec = len_sec; }
                                            ui.label("Start (s):");
                                            let chs = ui.add(egui::DragValue::new(&mut s_sec).range(0.0..=len_sec).speed(0.05).fixed_decimals(3)).changed();
                                            ui.label("End (s):");
                                            let che = ui.add(egui::DragValue::new(&mut e_sec).range(0.0..=len_sec).speed(0.05).fixed_decimals(3)).changed();
                                            if chs || che {
                                                let mut s = ((s_sec.max(0.0)) * sr).round() as usize;
                                                let mut e = ((e_sec.max(0.0)) * sr).round() as usize;
                                                if e < s { std::mem::swap(&mut s, &mut e); }
                                                s = s.min(tab.samples_len);
                                                e = e.min(tab.samples_len);
                                                tab.trim_range = Some((s, e));
                                                tab.selection = Some((s, e));
                                                range_opt = tab.trim_range;
                                                if preview_ok && e > s {
                                                    let mut mono = Self::editor_mixdown_mono(tab);
                                                    mono = mono[s..e].to_vec();
                                                    pending_preview = Some((ToolKind::Trim, mono));
                                                    stop_playback = true;
                                                    tab.preview_audio_tool = Some(ToolKind::Trim);
                                                } else {
                                                    tab.preview_audio_tool = None;
                                                    tab.preview_overlay = None;
                                                }
                                            }
                                        });
                                        // A/B setters from playhead
                                        ui.horizontal_wrapped(|ui| {
                                            if ui.button("Set A").on_hover_text("Set A at playhead").clicked() {
                                                let pos = playhead_display_now;
                                            let new_r = match tab.trim_range { None => Some((pos, pos)), Some((_a,b)) => Some((pos.min(b), pos.max(b))) };
                                                tab.trim_range = new_r;
                                                if let Some((a,b)) = tab.trim_range { if b>a {
                                                    if preview_ok {
                                                        // live preview: keep-only A?B
                                                        let mut mono = Self::editor_mixdown_mono(tab);
                                                        mono = mono[a..b].to_vec();
                                                        pending_preview = Some((ToolKind::Trim, mono));
                                                        stop_playback = true;
                                                        tab.preview_audio_tool = Some(ToolKind::Trim);
                                                    } else {
                                                        tab.preview_audio_tool = None;
                                                        tab.preview_overlay = None;
                                                    }
                                                } }
                                            }
                                            if ui.button("Set B").on_hover_text("Set B at playhead").clicked() {
                                                let pos = playhead_display_now;
                                            let new_r = match tab.trim_range { None => Some((pos, pos)), Some((a,_b)) => Some((a.min(pos), a.max(pos))) };
                                                tab.trim_range = new_r;
                                                if let Some((a,b)) = tab.trim_range { if b>a {
                                                    if preview_ok {
                                                        let mut mono = Self::editor_mixdown_mono(tab);
                                                        mono = mono[a..b].to_vec();
                                                        pending_preview = Some((ToolKind::Trim, mono));
                                                        stop_playback = true;
                                                        tab.preview_audio_tool = Some(ToolKind::Trim);
                                                    } else {
                                                        tab.preview_audio_tool = None;
                                                        tab.preview_overlay = None;
                                                    }
                                                } }
                                            }
                                            if ui.button("Clear").clicked() { tab.trim_range = None; need_restore_preview = true; }
                                        });
                                        range_opt = tab.trim_range;
                                        // Actions
                                        ui.horizontal_wrapped(|ui| {
                                        let dis = !range_opt.map(|(s,e)| e> s).unwrap_or(false);
                                        let range = range_opt.unwrap_or((0,0));
                                        if ui.add_enabled(!dis, egui::Button::new("Cut+Join")).clicked() { do_cutjoin = Some(range); }
                                        if ui.add_enabled(!dis, egui::Button::new("Apply Keep A?B")).clicked() { do_trim = Some(range); tab.preview_audio_tool=None; }
                                    });
                                    });
                                }
                                ToolKind::Fade => {
                                    // Simplified: duration (seconds) from start/end + Apply
                                    ui.scope(|ui| {
                                        let s = ui.style_mut();
                                        s.spacing.item_spacing = egui::vec2(6.0, 6.0);
                                        s.spacing.button_padding = egui::vec2(6.0, 3.0);
                                        if !preview_ok {
                                            ui.label(RichText::new("Preview disabled for large clips").weak());
                                        }
                                        let sr = self.audio.shared.out_sample_rate.max(1) as f32;
                                        let shape_label = |shape: crate::app::types::FadeShape| match shape {
                                            crate::app::types::FadeShape::Linear => "Linear",
                                            crate::app::types::FadeShape::EqualPower => "Equal",
                                            crate::app::types::FadeShape::Cosine => "Cosine",
                                            crate::app::types::FadeShape::SCurve => "S-Curve",
                                            crate::app::types::FadeShape::Quadratic => "Quadratic",
                                            crate::app::types::FadeShape::Cubic => "Cubic",
                                        };
                                        // Fade In
                                        ui.label("Fade In");
                                        ui.horizontal_wrapped(|ui| {
                                            let mut secs = tab.tool_state.fade_in_ms / 1000.0;
                                            if !secs.is_finite() { secs = 0.0; }
                                            ui.label("duration (s)");
                                            let mut changed = ui
                                                .add(
                                                    egui::DragValue::new(&mut secs)
                                                        .range(0.0..=600.0)
                                                        .speed(0.05)
                                                        .fixed_decimals(2),
                                                )
                                                .changed();
                                            ui.label("shape");
                                            let mut shape = tab.fade_in_shape;
                                            egui::ComboBox::from_id_salt("fade_in_shape")
                                                .selected_text(shape_label(shape))
                                                .show_ui(ui, |ui| {
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::Linear,
                                                        "Linear",
                                                    );
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::EqualPower,
                                                        "Equal",
                                                    );
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::Cosine,
                                                        "Cosine",
                                                    );
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::SCurve,
                                                        "S-Curve",
                                                    );
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::Quadratic,
                                                        "Quadratic",
                                                    );
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::Cubic,
                                                        "Cubic",
                                                    );
                                                });
                                            if shape != tab.fade_in_shape {
                                                tab.fade_in_shape = shape;
                                                changed = true;
                                            }
                                            if changed {
                                                tab.tool_state = ToolState{ fade_in_ms: (secs*1000.0).max(0.0), ..tab.tool_state };
                                                if preview_ok {
                                                    // Live preview (per-channel overlay) + mono audition
                                                    let n = ((secs) * sr).round() as usize;
                                                    // Build overlay per channel
                                                    let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                    for ch in overlay.iter_mut() {
                                                        let nn = n.min(ch.len());
                                                        for i in 0..nn { let t = i as f32 / nn.max(1) as f32; let w = Self::fade_weight(tab.fade_in_shape, t); ch[i] *= w; }
                                                    }
                                                    let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(tab.samples_len);
                                                    tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                                                        overlay.clone(),
                                                        ToolKind::Fade,
                                                        timeline_len,
                                                    ));
                                                    // Mono audition
                                                    let mut mono = Self::editor_mixdown_mono(tab);
                                                    let nn = n.min(mono.len());
                                                    for i in 0..nn { let t = i as f32 / nn.max(1) as f32; let w = Self::fade_weight(tab.fade_in_shape, t); mono[i] *= w; }
                                                    pending_preview = Some((ToolKind::Fade, mono));
                                                    stop_playback = true;
                                                    tab.preview_audio_tool = Some(ToolKind::Fade);
                                                } else {
                                                    tab.preview_audio_tool = None;
                                                    tab.preview_overlay = None;
                                                }
                                            }
                                            if ui.add_enabled(secs>0.0, egui::Button::new("Apply")).clicked() {
                                                let n = ((secs) * sr).round() as usize;
                                                do_fade_in = Some(((0, n.min(tab.samples_len)), tab.fade_in_shape));
                                                tab.preview_audio_tool = None; // will be rebuilt from destructive result below
                                                tab.preview_overlay = None;
                                                tab.tool_state = ToolState { fade_in_ms: 0.0, ..tab.tool_state };
                                            }
                                        });
                                        ui.separator();
                                        // Fade Out
                                        ui.label("Fade Out");
                                        ui.horizontal_wrapped(|ui| {
                                            let mut secs = tab.tool_state.fade_out_ms / 1000.0;
                                            if !secs.is_finite() { secs = 0.0; }
                                            ui.label("duration (s)");
                                            let mut changed = ui
                                                .add(
                                                    egui::DragValue::new(&mut secs)
                                                        .range(0.0..=600.0)
                                                        .speed(0.05)
                                                        .fixed_decimals(2),
                                                )
                                                .changed();
                                            ui.label("shape");
                                            let mut shape = tab.fade_out_shape;
                                            egui::ComboBox::from_id_salt("fade_out_shape")
                                                .selected_text(shape_label(shape))
                                                .show_ui(ui, |ui| {
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::Linear,
                                                        "Linear",
                                                    );
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::EqualPower,
                                                        "Equal",
                                                    );
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::Cosine,
                                                        "Cosine",
                                                    );
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::SCurve,
                                                        "S-Curve",
                                                    );
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::Quadratic,
                                                        "Quadratic",
                                                    );
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::Cubic,
                                                        "Cubic",
                                                    );
                                                });
                                            if shape != tab.fade_out_shape {
                                                tab.fade_out_shape = shape;
                                                changed = true;
                                            }
                                            if changed {
                                                tab.tool_state = ToolState{ fade_out_ms: (secs*1000.0).max(0.0), ..tab.tool_state };
                                                if preview_ok {
                                                    let n = ((secs) * sr).round() as usize;
                                                    // per-channel overlay
                                                    let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                    for ch in overlay.iter_mut() {
                                                        let len = ch.len(); let nn = n.min(len);
                                                        for i in 0..nn { let t = i as f32 / nn.max(1) as f32; let w = Self::fade_weight_out(tab.fade_out_shape, t); let idx = len - nn + i; ch[idx] *= w; }
                                                    }
                                                    let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(tab.samples_len);
                                                    tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                                                        overlay.clone(),
                                                        ToolKind::Fade,
                                                        timeline_len,
                                                    ));
                                                    // mono audition
                                                    let mut mono = Self::editor_mixdown_mono(tab);
                                                    let len = mono.len(); let nn = n.min(len);
                                                    for i in 0..nn { let t = i as f32 / nn.max(1) as f32; let w = Self::fade_weight_out(tab.fade_out_shape, t); let idx = len - nn + i; mono[idx] *= w; }
                                                    pending_preview = Some((ToolKind::Fade, mono));
                                                    stop_playback = true;
                                                    tab.preview_audio_tool = Some(ToolKind::Fade);
                                                } else {
                                                    tab.preview_audio_tool = None;
                                                    tab.preview_overlay = None;
                                                }
                                            }
                                            if ui.add_enabled(secs>0.0, egui::Button::new("Apply")).clicked() {
                                                let n = ((secs) * sr).round() as usize;
                                                do_fade_out = Some(((0, n.min(tab.samples_len)), tab.fade_out_shape));
                                                tab.preview_audio_tool = None;
                                                tab.preview_overlay = None;
                                                tab.tool_state = ToolState { fade_out_ms: 0.0, ..tab.tool_state };
                                            }
                                        });
                                    });
                                }
                                ToolKind::PitchShift => {
                                    ui.scope(|ui| {
                                        let s = ui.style_mut(); s.spacing.item_spacing = egui::vec2(6.0,6.0); s.spacing.button_padding = egui::vec2(6.0,3.0);
                                        if !preview_ok {
                                            ui.label(RichText::new("Large clip: preview runs in background").weak());
                                        }
                                        let mut semi = tab.tool_state.pitch_semitones;
                                        if !semi.is_finite() { semi = 0.0; }
                                        ui.label("Semitones");
                                        let changed = ui.add(egui::DragValue::new(&mut semi).range(-12.0..=12.0).speed(0.1).fixed_decimals(2)).changed();
                                        if changed {
                                            tab.tool_state = ToolState{ pitch_semitones: semi, ..tab.tool_state };
                                            stop_playback = true;
                                            tab.preview_audio_tool = Some(ToolKind::PitchShift);
                                            tab.preview_overlay = None;
                                            if preview_ok || tab.dirty {
                                                let mono = Self::editor_mixdown_mono(tab);
                                                pending_heavy_preview = Some((ToolKind::PitchShift, mono, semi));
                                                // Defer overlay spawn to avoid nested &mut borrow
                                                pending_overlay_job = Some((ToolKind::PitchShift, semi));
                                            } else {
                                                let path = tab.path.clone();
                                                pending_heavy_preview_path = Some((ToolKind::PitchShift, path.clone(), semi));
                                                pending_overlay_path = Some((ToolKind::PitchShift, path, semi));
                                            }
                                        }
                                        if overlay_busy || apply_busy { ui.add(egui::Spinner::new()); }
                                        if ui.add_enabled(!apply_busy, egui::Button::new("Apply")).clicked() {
                                            pending_pitch_apply = Some(tab.tool_state.pitch_semitones);
                                            tab.tool_state = ToolState { pitch_semitones: 0.0, ..tab.tool_state };
                                            tab.preview_audio_tool = None;
                                            tab.preview_overlay = None;
                                        }
                                    });
                                }
                                ToolKind::TimeStretch => {
                                    ui.scope(|ui| {
                                        let s = ui.style_mut(); s.spacing.item_spacing = egui::vec2(6.0,6.0); s.spacing.button_padding = egui::vec2(6.0,3.0);
                                        if !preview_ok {
                                            ui.label(RichText::new("Large clip: preview runs in background").weak());
                                        }
                                        let mut rate = tab.tool_state.stretch_rate;
                                        if !rate.is_finite() { rate = 1.0; }
                                        ui.label("Rate");
                                        let changed = ui.add(egui::DragValue::new(&mut rate).range(0.25..=4.0).speed(0.02).fixed_decimals(2)).changed();
                                        if changed {
                                            tab.tool_state = ToolState{ stretch_rate: rate, ..tab.tool_state };
                                            stop_playback = true;
                                            tab.preview_audio_tool = Some(ToolKind::TimeStretch);
                                            tab.preview_overlay = None;
                                            if preview_ok || tab.dirty {
                                                let mono = Self::editor_mixdown_mono(tab);
                                                pending_heavy_preview = Some((ToolKind::TimeStretch, mono, rate));
                                                // Defer overlay spawn to avoid nested &mut borrow
                                                pending_overlay_job = Some((ToolKind::TimeStretch, rate));
                                            } else {
                                                let path = tab.path.clone();
                                                pending_heavy_preview_path = Some((ToolKind::TimeStretch, path.clone(), rate));
                                                pending_overlay_path = Some((ToolKind::TimeStretch, path, rate));
                                            }
                                        }
                                        if overlay_busy || apply_busy { ui.add(egui::Spinner::new()); }
                                        if ui.add_enabled(!apply_busy, egui::Button::new("Apply")).clicked() {
                                            pending_stretch_apply = Some(tab.tool_state.stretch_rate);
                                            tab.tool_state = ToolState { stretch_rate: 1.0, ..tab.tool_state };
                                            tab.preview_audio_tool = None;
                                            tab.preview_overlay = None;
                                        }
                                    });
                                }
                                ToolKind::Gain => {
                                    if !preview_ok {
                                        ui.label(RichText::new("Preview disabled for large clips").weak());
                                    }
                                    let st = tab.tool_state;
                                    let mut gain_db = st.gain_db;
                                    if !gain_db.is_finite() { gain_db = 0.0; }
                                    ui.label("Gain (dB)"); ui.add(egui::DragValue::new(&mut gain_db).range(-24.0..=24.0).speed(0.1));
                                    tab.tool_state = ToolState{ gain_db, ..tab.tool_state };
                                    // live preview on change
                                    if (gain_db - st.gain_db).abs() > 1e-6 {
                                        if preview_ok {
                                            let g = db_to_amp(gain_db);
                                            // per-channel overlay
                                            let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                            for ch in overlay.iter_mut() { for v in ch.iter_mut() { *v *= g; } }
                                            let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(tab.samples_len);
                                            tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                                                overlay,
                                                ToolKind::Gain,
                                                timeline_len,
                                            ));
                                            // mono audition
                                            let mut mono = Self::editor_mixdown_mono(tab);
                                            for v in &mut mono { *v *= g; }
                                            pending_preview = Some((ToolKind::Gain, mono));
                                            stop_playback = true;
                                            tab.preview_audio_tool = Some(ToolKind::Gain);
                                        } else {
                                            tab.preview_audio_tool = None;
                                            tab.preview_overlay = None;
                                        }
                                    }
                                    if ui.button("Apply").clicked() {
                                        do_gain = Some(((0, tab.samples_len), gain_db));
                                        tab.preview_audio_tool = None;
                                        tab.preview_overlay = None;
                                        tab.tool_state = ToolState { gain_db: 0.0, ..tab.tool_state };
                                    }
                                }
                                ToolKind::Normalize => {
                                    if !preview_ok {
                                        ui.label(RichText::new("Preview disabled for large clips").weak());
                                    }
                                    let st = tab.tool_state;
                                    let mut target_db = st.normalize_target_db;
                                    if !target_db.is_finite() { target_db = -6.0; }
                                    ui.label("Target dBFS"); ui.add(egui::DragValue::new(&mut target_db).range(-24.0..=0.0).speed(0.1));
                                    tab.tool_state = ToolState{ normalize_target_db: target_db, ..tab.tool_state };
                                    let mut preview_normalize = |target_db: f32, tab: &mut EditorTab| {
                                        let mut mono = Self::editor_mixdown_mono(tab);
                                        if !mono.is_empty() {
                                            let mut peak = 0.0f32;
                                            for &v in &mono { peak = peak.max(v.abs()); }
                                            if peak > 0.0 {
                                                let g = db_to_amp(target_db) / peak.max(1e-12);
                                                // per-channel overlay
                                                let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                for ch in overlay.iter_mut() { for v in ch.iter_mut() { *v *= g; } }
                                                let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(tab.samples_len);
                                                tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                                                    overlay,
                                                    ToolKind::Normalize,
                                                    timeline_len,
                                                ));
                                                // mono audition
                                                for v in &mut mono { *v *= g; }
                                                pending_preview = Some((ToolKind::Normalize, mono));
                                                stop_playback = true;
                                                tab.preview_audio_tool = Some(ToolKind::Normalize);
                                            }
                                        }
                                    };
                                    if preview_ok {
                                        let changed = (target_db - st.normalize_target_db).abs() > 1e-6;
                                        if changed {
                                            preview_normalize(target_db, tab);
                                        }
                                    } else {
                                        tab.preview_audio_tool = None;
                                        tab.preview_overlay = None;
                                    }
                                    if ui.add_enabled(preview_ok, egui::Button::new("Preview")).clicked() {
                                        preview_normalize(target_db, tab);
                                    }
                                    if ui.button("Apply").clicked() {
                                        do_normalize = Some(((0, tab.samples_len), target_db));
                                        tab.preview_audio_tool = None;
                                        tab.preview_overlay = None;
                                        tab.tool_state =
                                            ToolState { normalize_target_db: -6.0, ..tab.tool_state };
                                    }
                                }
                                ToolKind::Loudness => {
                                    if !preview_ok {
                                        ui.label(RichText::new("Preview disabled for large clips").weak());
                                    }
                                    let st = tab.tool_state;
                                    let mut target_lufs = st.loudness_target_lufs;
                                    if !target_lufs.is_finite() { target_lufs = -14.0; }
                                    ui.label("Target LUFS (I)");
                                    ui.add(
                                        egui::DragValue::new(&mut target_lufs)
                                            .range(-36.0..=0.0)
                                            .speed(0.1),
                                    );
                                    tab.tool_state = ToolState {
                                        loudness_target_lufs: target_lufs,
                                        ..tab.tool_state
                                    };
                                    if ui.add_enabled(preview_ok, egui::Button::new("Preview")).clicked() {
                                        if let Ok(lufs) = crate::wave::lufs_integrated_from_multi(
                                            &tab.ch_samples,
                                            self.audio.shared.out_sample_rate,
                                        ) {
                                            if lufs.is_finite() {
                                                let gain_db = target_lufs - lufs;
                                                let gain = db_to_amp(gain_db);
                                                let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                for ch in overlay.iter_mut() {
                                                    for v in ch.iter_mut() {
                                                        *v = (*v * gain).clamp(-1.0, 1.0);
                                                    }
                                                }
                                                let timeline_len = overlay
                                                    .get(0)
                                                    .map(|c| c.len())
                                                    .unwrap_or(tab.samples_len);
                                                tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                                                    overlay,
                                                    ToolKind::Loudness,
                                                    timeline_len,
                                                ));
                                                let mut mono = Self::editor_mixdown_mono(tab);
                                                for v in &mut mono {
                                                    *v = (*v * gain).clamp(-1.0, 1.0);
                                                }
                                                pending_preview = Some((ToolKind::Loudness, mono));
                                                stop_playback = true;
                                                tab.preview_audio_tool = Some(ToolKind::Loudness);
                                            }
                                        }
                                    }
                                    if ui.button("Apply").clicked() {
                                        pending_loudness_apply = Some(target_lufs);
                                        tab.preview_audio_tool = None;
                                        tab.preview_overlay = None;
                                        tab.tool_state = ToolState {
                                            loudness_target_lufs: -14.0,
                                            ..tab.tool_state
                                        };
                                    }
                                }
                                ToolKind::Reverse => {
                                    if !preview_ok {
                                        ui.label(RichText::new("Preview disabled for large clips").weak());
                                    }
                                    ui.horizontal_wrapped(|ui| {
                                        if ui.add_enabled(preview_ok, egui::Button::new("Preview")).clicked() {
                                            // per-channel overlay
                                            let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                            for ch in overlay.iter_mut() { ch.reverse(); }
                                            let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(tab.samples_len);
                                            tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                                                overlay,
                                                ToolKind::Reverse,
                                                timeline_len,
                                            ));
                                            // mono audition
                                            let mut mono = Self::editor_mixdown_mono(tab);
                                            mono.reverse();
                                            pending_preview = Some((ToolKind::Reverse, mono));
                                            stop_playback = true;
                                            tab.preview_audio_tool = Some(ToolKind::Reverse);
                                        }
                                        if ui.button("Apply").clicked() { do_reverse = Some((0, tab.samples_len)); tab.preview_audio_tool=None; tab.preview_overlay=None; }
                                        if ui.button("Cancel").clicked() { need_restore_preview = true; }
                                    });
                                }
                            }
                        }
                        ViewMode::Spectrogram | ViewMode::Mel => {
                            ui.label(RichText::new("Display").strong());
                            ui.checkbox(&mut tab.show_waveform_overlay, "Waveform overlay");
                        }
                    }
                }); // end inspector
                if need_restore_preview {
                    pending_preview = None;
                    pending_heavy_preview = None;
                    pending_heavy_preview_path = None;
                    pending_overlay_job = None;
                    pending_overlay_path = None;
                }
                if let Some((tool, mono, p)) = pending_heavy_preview {
                    if self.is_decode_failed_path(&self.tabs[tab_idx].path) {
                        if let Some(tab) = self.tabs.get_mut(tab_idx) {
                            tab.preview_audio_tool = None;
                            tab.preview_overlay = None;
                        }
                    } else {
                        self.spawn_heavy_preview_owned(mono, tool, p);
                    }
                }
                if let Some((tool, path, p)) = pending_heavy_preview_path {
                    if self.is_decode_failed_path(&path) {
                        if let Some(tab) = self.tabs.get_mut(tab_idx) {
                            tab.preview_audio_tool = None;
                            tab.preview_overlay = None;
                        }
                    } else {
                        self.spawn_heavy_preview_from_path(path, tool, p);
                    }
                }
                if let Some(semi) = pending_pitch_apply {
                    self.spawn_editor_apply_for_tab(tab_idx, ToolKind::PitchShift, semi);
                }
                if let Some(rate) = pending_stretch_apply {
                    self.spawn_editor_apply_for_tab(tab_idx, ToolKind::TimeStretch, rate);
                }
                if let Some(target) = pending_loudness_apply {
                    self.spawn_editor_apply_for_tab(tab_idx, ToolKind::Loudness, target);
                }
                if stop_playback { self.audio.stop(); }
                if need_restore_preview { self.clear_preview_if_any(tab_idx); }
                if let Some(s) = request_seek { self.audio.seek_to_sample(s); }
                if let Some((tool_kind, mono)) = pending_preview { self.set_preview_mono(tab_idx, tool_kind, mono); }
            }); // end horizontal split
        if touch_spectro_cache {
            self.touch_spectro_cache(&spec_path);
        }

        if cancel_apply {
            self.cancel_editor_apply();
        }
        if cancel_decode {
            self.cancel_editor_decode();
        }
        if cancel_processing {
            self.cancel_processing();
        }
        if cancel_preview {
            self.cancel_heavy_preview();
        }
        if cancel_spectro {
            self.cancel_spectrogram_for_path(&tab_path);
        }

        // perform pending actions after borrows end
        // Defer starting heavy overlay until after UI to avoid nested &mut self borrow (E0499)
        if let Some((tool, p)) = pending_overlay_job {
            if !self.is_decode_failed_path(&self.tabs[tab_idx].path) {
                self.spawn_heavy_overlay_for_tab(tab_idx, tool, p);
            }
        }
        if let Some((tool, path, p)) = pending_overlay_path {
            if !self.is_decode_failed_path(&path) {
                self.spawn_heavy_overlay_from_path(path, tool, p);
            }
        }
        if request_undo {
            self.clear_preview_if_any(tab_idx);
            self.editor_apply_state = None;
            self.undo_in_tab(tab_idx);
        }
        if request_redo {
            self.clear_preview_if_any(tab_idx);
            self.editor_apply_state = None;
            self.redo_in_tab(tab_idx);
        }
        if let Some((s, e)) = do_set_loop_from {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                if s == 0 && e == 0 {
                    tab.loop_region = None;
                } else {
                    tab.loop_region = Some((s, e));
                    if tab.loop_mode == LoopMode::Marker {
                        self.audio.set_loop_enabled(true);
                        self.audio.set_loop_region(s, e);
                    }
                }
                Self::update_loop_markers_dirty(tab);
            }
        }
        if let Some(state) = pending_edit_undo.take() {
            self.push_editor_undo_state(tab_idx, state, true);
        }
        if let Some((s, e)) = do_trim {
            self.editor_apply_trim_range(tab_idx, (s, e));
        }
        if let Some(((s, e), in_ms, out_ms)) = do_fade {
            self.editor_apply_fade_range(tab_idx, (s, e), in_ms, out_ms);
        }
        if let Some(((s, e), shp)) = do_fade_in {
            self.editor_apply_fade_in_explicit(tab_idx, (s, e), shp);
        }
        if let Some(((mut s, mut e), shp)) = do_fade_out {
            // If range provided is (0, n) as length, anchor to end
            if let Some(tab) = self.tabs.get(tab_idx) {
                let len = tab.samples_len;
                if s == 0 {
                    s = len.saturating_sub(e);
                    e = len;
                }
            }
            self.editor_apply_fade_out_explicit(tab_idx, (s, e), shp);
        }
        if let Some(((s, e), gdb)) = do_gain {
            self.editor_apply_gain_range(tab_idx, (s, e), gdb);
        }
        if let Some(((s, e), tdb)) = do_normalize {
            self.editor_apply_normalize_range(tab_idx, (s, e), tdb);
        }
        if let Some((s, e)) = do_reverse {
            self.editor_apply_reverse_range(tab_idx, (s, e));
        }
        if let Some((_, _)) = do_cutjoin {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                tab.trim_range = None;
            }
        }
        if let Some((s, e)) = do_cutjoin {
            self.editor_delete_range_and_join(tab_idx, (s, e));
        }
        if do_commit_loop {
            let mut apply_xfade = false;
            let mut do_unwrap: Option<u32> = None;
            let mut undo_state = None;
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                if let Some(repeat) = tab.pending_loop_unwrap {
                    do_unwrap = Some(repeat);
                } else {
                    let will_change = tab.loop_region_committed != tab.loop_region
                        || tab.loop_region_applied != tab.loop_region
                        || tab.loop_xfade_samples > 0;
                    if will_change {
                        undo_state = Some(Self::capture_undo_state(tab));
                    }
                    tab.loop_region_committed = tab.loop_region;
                    tab.loop_region_applied = tab.loop_region_committed;
                    apply_xfade = tab.loop_xfade_samples > 0;
                }
                tab.pending_loop_unwrap = None;
                tab.preview_audio_tool = None;
                tab.preview_overlay = None;
            }
            if let Some(state) = undo_state {
                self.push_editor_undo_state(tab_idx, state, true);
            }
            if let Some(repeat) = do_unwrap {
                self.editor_apply_loop_unwrap(tab_idx, repeat);
            } else {
                if apply_xfade {
                    self.editor_apply_loop_xfade(tab_idx);
                }
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    Self::update_loop_markers_dirty(tab);
                }
            }
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                tab.loop_xfade_samples = 0;
                tab.tool_state = ToolState { loop_repeat: 2, ..tab.tool_state };
            }
        }
        if let Some(repeat) = do_preview_unwrap {
            let preview_ok = self
                .tabs
                .get(tab_idx)
                .map(|t| t.samples_len <= LIVE_PREVIEW_SAMPLE_LIMIT)
                .unwrap_or(false);
            if preview_ok {
                if let Some(tab) = self.tabs.get(tab_idx) {
                    if let Some(chans) = self.editor_preview_loop_unwrap(tab, repeat) {
                        let timeline_len = chans.get(0).map(|c| c.len()).unwrap_or(0);
                        let mono = Self::mixdown_channels(&chans, timeline_len);
                        let markers = Self::build_loop_unwrap_markers(
                            &tab.markers,
                            tab.loop_region.map(|v| v.0).unwrap_or(0),
                            tab.loop_region.map(|v| v.1).unwrap_or(0),
                            tab.samples_len,
                            repeat as usize,
                        );
                        if let Some(tab_mut) = self.tabs.get_mut(tab_idx) {
                            tab_mut.markers = markers;
                            tab_mut.preview_overlay = Some(Self::preview_overlay_from_channels(
                                chans,
                                ToolKind::LoopEdit,
                                timeline_len,
                            ));
                        }
                        if !mono.is_empty() {
                            self.set_preview_mono(tab_idx, ToolKind::LoopEdit, mono);
                        }
                    }
                }
            }
        }
        if do_commit_markers {
            let mut undo_state = None;
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                if tab.markers != tab.markers_committed {
                    undo_state = Some(Self::capture_undo_state(tab));
                }
                tab.markers_committed = tab.markers.clone();
                tab.markers_applied = tab.markers_committed.clone();
                tab.markers_dirty = tab.markers_committed != tab.markers_saved;
            }
            if let Some(state) = undo_state {
                self.push_editor_undo_state(tab_idx, state, true);
            }
        }
        if apply_pending_loop {
            if let Some(tab_ro) = self.tabs.get(tab_idx) {
                self.apply_loop_mode_for_tab(tab_ro);
            }
        }
    }
}
