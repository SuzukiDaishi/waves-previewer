use std::io::Write;
use std::path::PathBuf;

use super::types::{
    DebugAction, DebugAutomation, DebugStep, EditorTab, FadeShape, LoopMode, LoopXfadeShape,
    ToolKind, ViewMode,
};
use super::{RateMode, WavesPreviewer};

impl WavesPreviewer {
    pub(super) fn default_screenshot_path(&mut self) -> PathBuf {
        let tag = if self.active_tab.is_some() {
            "editor"
        } else {
            "list"
        };
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let base = Self::default_screenshot_dir();
        let mut name = format!("shot_{}_{}", stamp, tag);
        if self.screenshot_seq > 0 {
            name.push_str(&format!("_{:02}", self.screenshot_seq));
        }
        self.screenshot_seq = self.screenshot_seq.wrapping_add(1);
        base.join(format!("{name}.png"))
    }

    pub(super) fn default_screenshot_dir() -> PathBuf {
        #[cfg(target_os = "windows")]
        {
            if let Some(home) = std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            {
                return home.join("Pictures").join("Screenshots");
            }
        }
        #[cfg(target_os = "macos")]
        {
            if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
                return home.join("Desktop");
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
                return home.join("Pictures").join("Screenshots");
            }
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }

    pub(super) fn request_screenshot(
        &mut self,
        ctx: &egui::Context,
        path: PathBuf,
        exit_after: bool,
    ) {
        if self.pending_screenshot.is_some() {
            return;
        }
        self.pending_screenshot = Some(path);
        self.exit_after_screenshot = exit_after;
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
    }

    pub(super) fn handle_screenshot_events(&mut self, ctx: &egui::Context) {
        let mut shot: Option<std::sync::Arc<egui::ColorImage>> = None;
        ctx.input(|i| {
            for ev in &i.events {
                if let egui::Event::Screenshot { image, .. } = ev {
                    shot = Some(image.clone());
                }
            }
        });
        if let Some(image) = shot {
            if let Some(path) = self.pending_screenshot.take() {
                if let Err(err) = crate::app::capture::save_color_image_png(&path, &image) {
                    eprintln!("screenshot failed: {err:?}");
                    self.debug_log(format!("screenshot failed: {err}"));
                } else {
                    eprintln!("screenshot saved: {}", path.display());
                    self.debug_log(format!("screenshot saved: {}", path.display()));
                }
                if self.exit_after_screenshot {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    self.exit_after_screenshot = false;
                }
            }
        }
    }

    pub(super) fn debug_log(&mut self, msg: impl Into<String>) {
        if !self.debug.cfg.enabled {
            return;
        }
        let msg = msg.into();
        self.debug.logs.push_back(msg.clone());
        while self.debug.logs.len() > 200 {
            self.debug.logs.pop_front();
        }
        if let Some(path) = self.debug.cfg.log_path.as_ref() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(f, "{msg}");
            }
        }
    }

    pub(super) fn debug_trace_input(&mut self, msg: impl Into<String>) {
        if !self.debug.cfg.enabled || !self.debug.input_trace_enabled {
            return;
        }
        let elapsed = self.debug.started_at.elapsed().as_secs_f32();
        let entry = format!("{elapsed:>7.2}s {}", msg.into());
        self.debug.input_trace.push_back(entry);
        while self.debug.input_trace.len() > self.debug.input_trace_max.max(1) {
            self.debug.input_trace.pop_front();
        }
    }

    pub(super) fn debug_trace_event(&mut self, msg: impl Into<String>) {
        if !self.debug.cfg.enabled || !self.debug.event_trace_enabled {
            return;
        }
        let elapsed = self.debug.started_at.elapsed().as_secs_f32();
        let entry = format!("{elapsed:>7.2}s {}", msg.into());
        self.debug.event_trace.push_back(entry);
        while self.debug.event_trace.len() > self.debug.event_trace_max.max(1) {
            self.debug.event_trace.pop_front();
        }
    }

    pub(super) fn debug_summary(&self) -> String {
        let selected = self
            .selected_path_buf()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none)".to_string());
        let active_tab = self
            .active_tab
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.display_name.clone())
            .unwrap_or_else(|| "(none)".to_string());
        let playing = self
            .audio
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed);
        let loop_enabled = self
            .audio
            .shared
            .loop_enabled
            .load(std::sync::atomic::Ordering::Relaxed);
        let processing = self.processing.is_some();
        let export = self.export_state.is_some();
        let decoding = self.editor_decode_state.is_some();
        let meta_pending = self.scan_in_progress || !self.meta_inflight.is_empty();
        let gain_dirty = self.pending_gain_count();
        let mut lines = Vec::new();
        lines.push(format!("files: {}/{}", self.files.len(), self.items.len()));
        lines.push(format!("selected: {selected}"));
        lines.push(format!("tabs: {} (active: {active_tab})", self.tabs.len()));
        lines.push(format!(
            "mode: {:?} rate {:.2} pitch {:.2}",
            self.mode, self.playback_rate, self.pitch_semitones
        ));
        lines.push(format!(
            "playing: {} loop: {} meter_db: {:.1}",
            playing, loop_enabled, self.meter_db
        ));
        lines.push(format!("pending_gains: {}", gain_dirty));
        lines.push(format!(
            "processing: {} export: {} decode: {}",
            processing, export, decoding
        ));
        lines.push(format!("meta_pending: {}", meta_pending));
        lines.join("\n")
    }

    pub(super) fn cancel_processing(&mut self) {
        self.processing = None;
        self.audio.stop();
        self.list_preview_rx = None;
    }

    pub(super) fn cancel_editor_decode(&mut self) {
        if let Some(state) = &self.editor_decode_state {
            state
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
            let path = state.path.clone();
            if let Some(idx) = self.tabs.iter().position(|t| t.path == path) {
                if let Some(tab) = self.tabs.get_mut(idx) {
                    tab.loading = false;
                }
            }
        }
        self.editor_decode_state = None;
    }

    pub(super) fn cancel_editor_apply(&mut self) {
        self.editor_apply_state = None;
    }

    pub(super) fn cancel_heavy_preview(&mut self) {
        self.heavy_preview_rx = None;
        self.heavy_preview_tool = None;
        self.heavy_overlay_rx = None;
        self.overlay_expected_tool = None;
    }

    pub(super) fn default_debug_summary_path(&mut self) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut name = format!("summary_{stamp}");
        if self.debug_summary_seq > 0 {
            name.push_str(&format!("_{:02}", self.debug_summary_seq));
        }
        self.debug_summary_seq = self.debug_summary_seq.wrapping_add(1);
        base.join("debug").join(format!("{name}.txt"))
    }

    pub(super) fn save_debug_summary(&mut self, path: PathBuf) {
        let summary = self.debug_summary();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&path, summary) {
            Ok(()) => self.debug_log(format!("debug summary saved: {}", path.display())),
            Err(err) => self.debug_log(format!("debug summary failed: {err}")),
        }
    }

    pub(super) fn debug_check_invariants(&mut self) {
        let mut issues = Vec::new();
        if let Some(i) = self.selected {
            if i >= self.files.len() {
                issues.push(format!("selected out of range: {i}"));
            }
        }
        if let Some(i) = self.active_tab {
            if i >= self.tabs.len() {
                issues.push(format!("active_tab out of range: {i}"));
            }
        }
        if let Some(i) = self.active_tab {
            if let Some(tab) = self.tabs.get(i) {
                if tab.view_offset > tab.samples_len {
                    issues.push(format!(
                        "view_offset > samples_len: {} > {}",
                        tab.view_offset, tab.samples_len
                    ));
                }
                if tab.samples_per_px <= 0.0 {
                    issues.push(format!("samples_per_px <= 0: {}", tab.samples_per_px));
                }
                for (ci, ch) in tab.ch_samples.iter().enumerate() {
                    if ch.len() != tab.samples_len {
                        issues.push(format!(
                            "channel {ci} len {} != {}",
                            ch.len(),
                            tab.samples_len
                        ));
                        break;
                    }
                }
                if let Some((a, b)) = tab.loop_region {
                    if a > tab.samples_len || b > tab.samples_len {
                        issues.push(format!(
                            "loop_region out of range: {a}..{b} len {}",
                            tab.samples_len
                        ));
                    }
                }
            }
        }
        if issues.is_empty() {
            self.debug_log("invariant check ok");
        } else {
            for issue in issues {
                self.debug_log(format!("invariant: {issue}"));
            }
        }
    }

    pub(super) fn setup_debug_automation(&mut self) {
        if !self.debug.cfg.auto_run {
            return;
        }
        let delay = self.debug.cfg.auto_run_delay_frames.max(1);
        let mut steps = std::collections::VecDeque::new();
        if self.debug.cfg.auto_run_editor {
            self.build_editor_debug_steps(&mut steps, delay);
            if self.debug.cfg.auto_run_exit {
                steps.push_back(DebugStep {
                    wait_frames: 1,
                    action: DebugAction::Exit,
                });
            }
            self.debug.auto = Some(DebugAutomation { steps });
            return;
        }
        let pitch = self.debug.cfg.auto_run_pitch_shift_semitones;
        let stretch = self.debug.cfg.auto_run_time_stretch_rate;
        if pitch.is_some() || stretch.is_some() {
            steps.push_back(DebugStep {
                wait_frames: delay,
                action: DebugAction::OpenFirst,
            });
            steps.push_back(DebugStep {
                wait_frames: delay,
                action: DebugAction::ScreenshotAuto,
            });
            if let Some(semi) = pitch {
                steps.push_back(DebugStep {
                    wait_frames: delay,
                    action: DebugAction::PreviewPitchShift(semi),
                });
                steps.push_back(DebugStep {
                    wait_frames: delay,
                    action: DebugAction::ScreenshotAuto,
                });
            }
            if let Some(rate) = stretch {
                steps.push_back(DebugStep {
                    wait_frames: delay,
                    action: DebugAction::PreviewTimeStretch(rate),
                });
                steps.push_back(DebugStep {
                    wait_frames: delay,
                    action: DebugAction::ScreenshotAuto,
                });
            }
            steps.push_back(DebugStep {
                wait_frames: delay,
                action: DebugAction::DumpSummaryAuto,
            });
            if self.debug.cfg.auto_run_exit {
                steps.push_back(DebugStep {
                    wait_frames: 1,
                    action: DebugAction::Exit,
                });
            }
            self.debug.auto = Some(DebugAutomation { steps });
            return;
        }
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::OpenFirst,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::DumpSummaryAuto,
        });
        if self.debug.cfg.auto_run_exit {
            steps.push_back(DebugStep {
                wait_frames: 1,
                action: DebugAction::Exit,
            });
        }
        self.debug.auto = Some(DebugAutomation { steps });
    }

    fn build_editor_debug_steps(
        &mut self,
        steps: &mut std::collections::VecDeque<DebugStep>,
        delay: u32,
    ) {
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::OpenFirst,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SetSelection {
                start_frac: 0.1,
                end_frac: 0.35,
            },
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SetActiveTool(ToolKind::LoopEdit),
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SetLoopRegion {
                start_frac: 0.2,
                end_frac: 0.6,
            },
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SetLoopXfade {
                ms: 40.0,
                shape: LoopXfadeShape::EqualPower,
            },
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SetLoopMode(LoopMode::Marker),
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ApplyLoopXfade,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::WriteLoopMarkers,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SetActiveTool(ToolKind::Markers),
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::AddMarker { frac: 0.25 },
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::AddMarker { frac: 0.75 },
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::WriteMarkers,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SetActiveTool(ToolKind::Trim),
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SetTrimRange {
                start_frac: 0.1,
                end_frac: 0.9,
            },
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ApplyTrim,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SetActiveTool(ToolKind::Fade),
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ApplyFadeIn {
                ms: 150.0,
                shape: FadeShape::SCurve,
            },
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ApplyFadeOut {
                ms: 150.0,
                shape: FadeShape::SCurve,
            },
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SetActiveTool(ToolKind::Gain),
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ApplyGain { db: -6.0 },
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SetActiveTool(ToolKind::Normalize),
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ApplyNormalize { db: -3.0 },
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SetActiveTool(ToolKind::Reverse),
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ApplyReverse,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SetActiveTool(ToolKind::PitchShift),
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::PreviewPitchShift(7.0),
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ApplyPitchShift(7.0),
        });
        steps.push_back(DebugStep {
            wait_frames: delay.saturating_mul(4),
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SetActiveTool(ToolKind::TimeStretch),
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::PreviewTimeStretch(1.25),
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ApplyTimeStretch(1.25),
        });
        steps.push_back(DebugStep {
            wait_frames: delay.saturating_mul(4),
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SetViewMode(ViewMode::Spectrogram),
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SetWaveformOverlay(false),
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SetViewMode(ViewMode::Mel),
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SetViewMode(ViewMode::Waveform),
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SetWaveformOverlay(true),
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::DumpSummaryAuto,
        });
    }

    pub(super) fn debug_tick(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.key_pressed(egui::Key::F12)) {
            self.debug.cfg.enabled = true;
            self.debug.show_window = !self.debug.show_window;
        }

        if !self.debug.cfg.enabled {
            return;
        }

        let raw_focused = ctx.input(|i| i.raw.focused);
        let events_len = ctx.input(|i| i.raw.events.len());
        let mods = ctx.input(|i| i.modifiers);
        let ctrl = mods.ctrl || mods.command;
        let pressed_c = ctx.input(|i| i.key_pressed(egui::Key::C));
        let pressed_v = ctx.input(|i| i.key_pressed(egui::Key::V));
        let down_c = ctx.input(|i| i.key_down(egui::Key::C));
        let down_v = ctx.input(|i| i.key_down(egui::Key::V));
        self.debug.last_raw_focused = raw_focused;
        self.debug.last_events_len = events_len;
        self.debug.last_ctrl_down = ctrl;
        self.debug.last_key_c_pressed = pressed_c;
        self.debug.last_key_v_pressed = pressed_v;
        self.debug.last_key_c_down = down_c;
        self.debug.last_key_v_down = down_v;
        if self.debug.event_trace_enabled {
            let events = ctx.input(|i| i.raw.events.clone());
            for ev in events {
                self.debug_trace_event(format!("event: {:?}", ev));
            }
        }
        let wants_kb = ctx.wants_keyboard_input();
        let wants_ptr = ctx.wants_pointer_input();
        if ctrl && (pressed_c || pressed_v) {
            let key = if pressed_c { "C" } else { "V" };
            let pos = ctx.input(|i| i.pointer.hover_pos());
            let pos_text = pos
                .map(|p| format!("{:.1},{:.1}", p.x, p.y))
                .unwrap_or_else(|| "(none)".to_string());
            let msg = format!(
                "hotkey Ctrl+{} (wants_kb={} wants_ptr={} pos={} mods=ctrl:{} shift:{} alt:{})",
                key,
                wants_kb,
                wants_ptr,
                pos_text,
                mods.ctrl || mods.command,
                mods.shift,
                mods.alt
            );
            self.debug.last_hotkey = Some(format!("Ctrl+{}", key));
            self.debug.last_hotkey_at = Some(std::time::Instant::now());
            self.debug_trace_input(msg);
        }

        if self.debug.check_counter > 0 {
            self.debug.check_counter = self.debug.check_counter.saturating_sub(1);
        } else {
            self.debug_check_invariants();
            self.debug.check_counter = self.debug.cfg.check_interval_frames.max(1);
        }

        self.debug_run_automation(ctx);
    }

    pub(super) fn debug_run_automation(&mut self, ctx: &egui::Context) {
        if self.files.is_empty() {
            return;
        }

        let action = {
            let Some(auto) = self.debug.auto.as_mut() else {
                return;
            };
            if let Some(step) = auto.steps.front_mut() {
                if step.wait_frames > 0 {
                    step.wait_frames = step.wait_frames.saturating_sub(1);
                    return;
                }
            }
            auto.steps.pop_front().map(|step| step.action)
        };

        if let Some(action) = action {
            self.execute_debug_action(ctx, action);
        }

        let completed = self
            .debug
            .auto
            .as_ref()
            .map(|auto| auto.steps.is_empty())
            .unwrap_or(false);

        if completed {
            self.debug.auto = None;
            self.debug_log("auto-run complete");
        }
    }

    pub(super) fn execute_debug_action(&mut self, ctx: &egui::Context, action: DebugAction) {
        match action {
            DebugAction::OpenFirst => {
                if self.active_tab.is_none() {
                    self.open_first_in_list();
                    self.debug_log("auto: open first");
                }
            }
            DebugAction::ScreenshotAuto => {
                let path = self.default_screenshot_path();
                self.request_screenshot(ctx, path, false);
                self.debug_log("auto: screenshot");
            }
            DebugAction::ScreenshotPath(path) => {
                self.request_screenshot(ctx, path, false);
                self.debug_log("auto: screenshot path");
            }
            DebugAction::SetActiveTool(tool) => {
                if let Some(tab_idx) = self.active_tab {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.active_tool = tool;
                        self.debug_log(format!("auto: tool {:?}", tool));
                    }
                    self.refresh_tool_preview_for_tab(tab_idx);
                }
            }
            DebugAction::SetSelection {
                start_frac,
                end_frac,
            } => {
                if let Some(tab_idx) = self.active_tab {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        if let Some((s, e)) = Self::debug_range_for_tab(tab, start_frac, end_frac) {
                            tab.selection = Some((s, e));
                            tab.drag_select_anchor = None;
                            self.debug_log(format!("auto: selection {s}..{e}"));
                        }
                    }
                }
            }
            DebugAction::SetTrimRange {
                start_frac,
                end_frac,
            } => {
                if let Some(tab_idx) = self.active_tab {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        if let Some((s, e)) = Self::debug_range_for_tab(tab, start_frac, end_frac) {
                            tab.trim_range = Some((s, e));
                            self.debug_log(format!("auto: trim_range {s}..{e}"));
                        }
                    }
                }
            }
            DebugAction::SetLoopRegion {
                start_frac,
                end_frac,
            } => {
                if let Some(tab_idx) = self.active_tab {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        if let Some((s, e)) = Self::debug_range_for_tab(tab, start_frac, end_frac) {
                            tab.loop_region = Some((s, e));
                            Self::update_loop_markers_dirty(tab);
                            self.debug_log(format!("auto: loop_region {s}..{e}"));
                        }
                    }
                }
            }
            DebugAction::SetLoopMode(mode) => {
                if let Some(tab_idx) = self.active_tab {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.loop_mode = mode;
                        self.debug_log(format!("auto: loop_mode {:?}", mode));
                    }
                    if let Some(tab) = self.tabs.get(tab_idx) {
                        self.apply_loop_mode_for_tab(tab);
                    }
                }
            }
            DebugAction::SetLoopXfade { ms, shape } => {
                if let Some(tab_idx) = self.active_tab {
                    let applied = if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        let sr = self.audio.shared.out_sample_rate.max(1) as f32;
                        let samp = ((ms / 1000.0) * sr).round().max(0.0) as usize;
                        tab.loop_xfade_samples = samp.min(tab.samples_len / 2);
                        tab.loop_xfade_shape = shape;
                        Some(tab.loop_xfade_samples)
                    } else {
                        None
                    };
                    if let Some(samples) = applied {
                        self.debug_log(format!(
                            "auto: loop_xfade {}ms ({} samples)",
                            ms, samples
                        ));
                    }
                    if let Some(tab) = self.tabs.get(tab_idx) {
                        self.apply_loop_mode_for_tab(tab);
                    }
                }
            }
            DebugAction::AddMarker { frac } => {
                if let Some(tab_idx) = self.active_tab {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        if tab.samples_len == 0 {
                            return;
                        }
                        let pos = ((tab.samples_len as f32) * frac)
                            .round()
                            .clamp(0.0, (tab.samples_len - 1) as f32) as usize;
                        let label = Self::next_marker_label(&tab.markers);
                        let entry = crate::markers::MarkerEntry {
                            label,
                            sample: pos,
                        };
                        match tab.markers.binary_search_by_key(&pos, |m| m.sample) {
                            Ok(idx) => tab.markers[idx] = entry,
                            Err(idx) => tab.markers.insert(idx, entry),
                        }
                        tab.markers_dirty = true;
                        self.debug_log(format!("auto: add marker @{}", pos));
                    }
                }
            }
            DebugAction::ClearMarkers => {
                if let Some(tab_idx) = self.active_tab {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.markers.clear();
                        tab.markers_dirty = true;
                        self.debug_log("auto: clear markers");
                    }
                }
            }
            DebugAction::WriteMarkers => {
                if let Some(tab_idx) = self.active_tab {
                    let ok = self.write_markers_for_tab(tab_idx);
                    self.debug_log(format!("auto: write markers {}", ok));
                }
            }
            DebugAction::WriteLoopMarkers => {
                if let Some(tab_idx) = self.active_tab {
                    let ok = self.write_loop_markers_for_tab(tab_idx);
                    self.debug_log(format!("auto: write loop markers {}", ok));
                }
            }
            DebugAction::ApplyTrim => {
                if let Some(tab_idx) = self.active_tab {
                    if let Some(range) = self.debug_pick_range(tab_idx) {
                        self.editor_apply_trim_range(tab_idx, range);
                        self.debug_log(format!("auto: trim {:?}", range));
                    }
                }
            }
            DebugAction::ApplyLoopXfade => {
                if let Some(tab_idx) = self.active_tab {
                    self.editor_apply_loop_xfade(tab_idx);
                    self.debug_log("auto: apply loop xfade");
                }
            }
            DebugAction::ApplyFadeIn { ms, shape } => {
                if let Some(tab_idx) = self.active_tab {
                    if let Some(range) = self.debug_pick_range(tab_idx) {
                        if let Some(tab) = self.tabs.get_mut(tab_idx) {
                            tab.fade_in_shape = shape;
                            tab.tool_state.fade_in_ms = ms;
                        }
                        self.editor_apply_fade_in_explicit(tab_idx, range, shape);
                        self.debug_log(format!("auto: fade in {:?} {:?}", range, shape));
                    }
                }
            }
            DebugAction::ApplyFadeOut { ms, shape } => {
                if let Some(tab_idx) = self.active_tab {
                    if let Some(range) = self.debug_pick_range(tab_idx) {
                        if let Some(tab) = self.tabs.get_mut(tab_idx) {
                            tab.fade_out_shape = shape;
                            tab.tool_state.fade_out_ms = ms;
                        }
                        self.editor_apply_fade_out_explicit(tab_idx, range, shape);
                        self.debug_log(format!("auto: fade out {:?} {:?}", range, shape));
                    }
                }
            }
            DebugAction::ApplyFadeRange {
                in_ms,
                out_ms,
                shape,
            } => {
                if let Some(tab_idx) = self.active_tab {
                    if let Some(range) = self.debug_pick_range(tab_idx) {
                        if let Some(tab) = self.tabs.get_mut(tab_idx) {
                            tab.fade_in_shape = shape;
                            tab.fade_out_shape = shape;
                            tab.tool_state.fade_in_ms = in_ms;
                            tab.tool_state.fade_out_ms = out_ms;
                        }
                        self.editor_apply_fade_range(tab_idx, range, in_ms, out_ms);
                        self.debug_log(format!("auto: fade range {:?}", range));
                    }
                }
            }
            DebugAction::ApplyGain { db } => {
                if let Some(tab_idx) = self.active_tab {
                    if let Some(range) = self.debug_pick_range(tab_idx) {
                        if let Some(tab) = self.tabs.get_mut(tab_idx) {
                            tab.tool_state.gain_db = db;
                        }
                        self.editor_apply_gain_range(tab_idx, range, db);
                        self.debug_log(format!("auto: gain {} dB", db));
                    }
                }
            }
            DebugAction::ApplyNormalize { db } => {
                if let Some(tab_idx) = self.active_tab {
                    if let Some(range) = self.debug_pick_range(tab_idx) {
                        if let Some(tab) = self.tabs.get_mut(tab_idx) {
                            tab.tool_state.normalize_target_db = db;
                        }
                        self.editor_apply_normalize_range(tab_idx, range, db);
                        self.debug_log(format!("auto: normalize {} dB", db));
                    }
                }
            }
            DebugAction::ApplyReverse => {
                if let Some(tab_idx) = self.active_tab {
                    if let Some(range) = self.debug_pick_range(tab_idx) {
                        self.editor_apply_reverse_range(tab_idx, range);
                        self.debug_log(format!("auto: reverse {:?}", range));
                    }
                }
            }
            DebugAction::ApplyPitchShift(semi) => {
                if let Some(tab_idx) = self.active_tab {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.tool_state.pitch_semitones = semi;
                    }
                    self.spawn_editor_apply_for_tab(tab_idx, ToolKind::PitchShift, semi);
                    self.debug_log(format!("auto: apply pitch shift {}", semi));
                }
            }
            DebugAction::ApplyTimeStretch(rate) => {
                if let Some(tab_idx) = self.active_tab {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.tool_state.stretch_rate = rate;
                    }
                    self.spawn_editor_apply_for_tab(tab_idx, ToolKind::TimeStretch, rate);
                    self.debug_log(format!("auto: apply time stretch {}", rate));
                }
            }
            DebugAction::SetViewMode(mode) => {
                if let Some(tab_idx) = self.active_tab {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.view_mode = mode;
                        self.debug_log(format!("auto: view mode {:?}", mode));
                    }
                }
            }
            DebugAction::SetWaveformOverlay(flag) => {
                if let Some(tab_idx) = self.active_tab {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.show_waveform_overlay = flag;
                        self.debug_log(format!("auto: waveform overlay {}", flag));
                    }
                }
            }
            DebugAction::ToggleMode => {
                self.mode = match self.mode {
                    RateMode::Speed => RateMode::PitchShift,
                    RateMode::PitchShift => RateMode::TimeStretch,
                    RateMode::TimeStretch => RateMode::Speed,
                };
                self.rebuild_current_buffer_with_mode();
                self.debug_log("auto: toggle mode");
            }
            DebugAction::PlayPause => {
                self.audio.toggle_play();
                self.debug_log("auto: play/pause");
            }
            DebugAction::SelectNext => {
                if let Some(cur) = self.selected {
                    let next = (cur + 1).min(self.files.len().saturating_sub(1));
                    self.select_and_load(next, true);
                    self.debug_log(format!("auto: select {next}"));
                }
            }
            DebugAction::PreviewTimeStretch(rate) => {
                if let Some(tab_idx) = self.active_tab {
                    let mono = {
                        let tab = &mut self.tabs[tab_idx];
                        tab.active_tool = ToolKind::TimeStretch;
                        tab.tool_state.stretch_rate = rate;
                        tab.preview_audio_tool = Some(ToolKind::TimeStretch);
                        tab.preview_overlay = None;
                        Self::editor_mixdown_mono(tab)
                    };
                    self.spawn_heavy_preview_owned(mono, ToolKind::TimeStretch, rate);
                    self.spawn_heavy_overlay_for_tab(tab_idx, ToolKind::TimeStretch, rate);
                    self.debug_log("auto: preview time stretch");
                } else {
                    self.debug_log("auto: preview time stretch skipped (no tab)");
                }
            }
            DebugAction::PreviewPitchShift(semi) => {
                if let Some(tab_idx) = self.active_tab {
                    let mono = {
                        let tab = &mut self.tabs[tab_idx];
                        tab.active_tool = ToolKind::PitchShift;
                        tab.tool_state.pitch_semitones = semi;
                        tab.preview_audio_tool = Some(ToolKind::PitchShift);
                        tab.preview_overlay = None;
                        Self::editor_mixdown_mono(tab)
                    };
                    self.spawn_heavy_preview_owned(mono, ToolKind::PitchShift, semi);
                    self.spawn_heavy_overlay_for_tab(tab_idx, ToolKind::PitchShift, semi);
                    self.debug_log("auto: preview pitch shift");
                } else {
                    self.debug_log("auto: preview pitch shift skipped (no tab)");
                }
            }
            DebugAction::DumpSummaryAuto => {
                let path = self.default_debug_summary_path();
                self.save_debug_summary(path);
            }
            DebugAction::Exit => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn debug_range_for_tab(
        tab: &EditorTab,
        start_frac: f32,
        end_frac: f32,
    ) -> Option<(usize, usize)> {
        if tab.samples_len == 0 {
            return None;
        }
        let mut s = (tab.samples_len as f32 * start_frac.clamp(0.0, 1.0)).floor() as usize;
        let mut e = (tab.samples_len as f32 * end_frac.clamp(0.0, 1.0)).ceil() as usize;
        if s > e {
            std::mem::swap(&mut s, &mut e);
        }
        if e <= s {
            e = (s + 1).min(tab.samples_len);
        }
        if s >= tab.samples_len {
            return None;
        }
        let e = e.min(tab.samples_len);
        Some((s, e))
    }

    fn debug_pick_range(&self, tab_idx: usize) -> Option<(usize, usize)> {
        let tab = self.tabs.get(tab_idx)?;
        if let Some(range) = tab.trim_range {
            if range.1 > range.0 {
                return Some(range);
            }
        }
        if let Some(range) = tab.selection {
            if range.1 > range.0 {
                return Some(range);
            }
        }
        Self::debug_range_for_tab(tab, 0.1, 0.4)
    }
}
