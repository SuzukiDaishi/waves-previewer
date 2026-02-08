use std::io::Write;
use std::path::{Path, PathBuf};

use super::types::{
    DebugAction, DebugAutomation, DebugStep, EditorTab, FadeShape, LoopMode, LoopXfadeShape,
    ToolKind, ViewMode,
};
use super::{RateMode, WavesPreviewer};

impl WavesPreviewer {
    pub(super) fn debug_start_external_merge_test(&mut self, rows: usize, cols: usize) {
        let rows = rows.max(1);
        let cols = cols.max(2);
        let base = std::path::PathBuf::from("debug");
        let path_a = base.join("external_dummy_a.csv");
        let path_b = base.join("external_dummy_b.csv");
        let has_header = true;
        if let Err(err) = write_debug_external_merge_csvs(&path_a, &path_b, rows, cols, has_header)
        {
            self.external_load_error = Some(err);
            return;
        }
        self.external_load_queue.clear();
        self.pending_external_restore = None;
        self.queue_external_load_with_current_settings(
            path_b,
            crate::app::external_ops::ExternalLoadTarget::New,
        );
        self.external_sheet_selected = None;
        self.external_sheet_names.clear();
        self.external_settings_dirty = false;
        self.external_load_target = Some(crate::app::external_ops::ExternalLoadTarget::New);
        self.show_external_dialog = true;
        self.external_load_error = None;
        self.begin_external_load(path_a);
        self.debug_log("external merge test started");
    }
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
        if self.debug.cfg.input_trace_to_console {
            println!(
                "{}",
                self.debug.input_trace.back().unwrap_or(&String::new())
            );
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

    fn debug_push_latency_sample(samples: &mut std::collections::VecDeque<f32>, value_ms: f32) {
        if !value_ms.is_finite() || value_ms < 0.0 {
            return;
        }
        samples.push_back(value_ms);
        while samples.len() > 512 {
            samples.pop_front();
        }
    }

    pub(super) fn debug_mark_list_select_start(&mut self, path: &Path) {
        self.debug.list_select_started_at = Some(std::time::Instant::now());
        self.debug.list_select_started_path = Some(path.to_path_buf());
    }

    pub(super) fn debug_mark_tab_switch_start(&mut self, path: &Path) {
        self.debug.tab_switch_started_at = Some(std::time::Instant::now());
        self.debug.tab_switch_started_path = Some(path.to_path_buf());
    }

    pub(super) fn debug_mark_tab_switch_interactive(&mut self, path: &Path) {
        let Some(started_at) = self.debug.tab_switch_started_at else {
            return;
        };
        if self
            .debug
            .tab_switch_started_path
            .as_deref()
            .map(|p| p == path)
            .unwrap_or(false)
        {
            let elapsed_ms = started_at.elapsed().as_secs_f32() * 1000.0;
            Self::debug_push_latency_sample(&mut self.debug.tab_switch_to_interactive_ms, elapsed_ms);
            self.debug.tab_switch_started_at = None;
            self.debug.tab_switch_started_path = None;
        }
    }

    pub(super) fn debug_push_ui_input_to_paint_sample(&mut self, value_ms: f32) {
        Self::debug_push_latency_sample(&mut self.debug.ui_input_to_paint_ms, value_ms);
    }

    pub(super) fn debug_push_metadata_probe_sample(&mut self, value_ms: f32) {
        Self::debug_push_latency_sample(&mut self.debug.metadata_probe_ms, value_ms);
    }

    pub(super) fn debug_push_bg_lufs_job_sample(&mut self, value_ms: f32) {
        Self::debug_push_latency_sample(&mut self.debug.bg_lufs_job_ms, value_ms);
    }

    pub(super) fn debug_push_bg_dbfs_job_sample(&mut self, value_ms: f32) {
        Self::debug_push_latency_sample(&mut self.debug.bg_dbfs_job_ms, value_ms);
    }

    pub(super) fn debug_push_src_resample_sample(&mut self, value_ms: f32) {
        Self::debug_push_latency_sample(&mut self.debug.src_resample_ms, value_ms);
    }

    pub(super) fn debug_mark_list_preview_ready(&mut self, path: &Path) {
        let Some(started_at) = self.debug.list_select_started_at else {
            return;
        };
        if self
            .debug
            .list_select_started_path
            .as_deref()
            .map(|p| p == path)
            .unwrap_or(false)
        {
            let elapsed_ms = started_at.elapsed().as_secs_f32() * 1000.0;
            Self::debug_push_latency_sample(&mut self.debug.select_to_preview_ms, elapsed_ms);
        }
    }

    pub(super) fn debug_mark_list_play_start(&mut self, path: &Path) {
        let Some(started_at) = self.debug.list_select_started_at else {
            return;
        };
        if self
            .debug
            .list_select_started_path
            .as_deref()
            .map(|p| p == path)
            .unwrap_or(false)
        {
            let elapsed_ms = started_at.elapsed().as_secs_f32() * 1000.0;
            Self::debug_push_latency_sample(&mut self.debug.select_to_play_ms, elapsed_ms);
            self.debug.list_select_started_at = None;
            self.debug.list_select_started_path = None;
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
        let avg_frame_ms = if self.debug.frame_samples > 0 {
            self.debug.frame_sum_ms / self.debug.frame_samples as f64
        } else {
            0.0
        };
        lines.push(format!(
            "frame_ms: last {:.2} avg {:.2} peak {:.2} samples {}",
            self.debug.frame_last_ms,
            avg_frame_ms,
            self.debug.frame_peak_ms,
            self.debug.frame_samples
        ));
        let summarize = |samples: &std::collections::VecDeque<f32>| -> String {
            if samples.is_empty() {
                return "n=0".to_string();
            }
            let mut values: Vec<f32> = samples.iter().copied().collect();
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let n = values.len();
            let p50_idx = ((n.saturating_sub(1)) as f32 * 0.50).round() as usize;
            let p95_idx = ((n.saturating_sub(1)) as f32 * 0.95).round() as usize;
            let p50 = values[p50_idx.min(n - 1)];
            let p95 = values[p95_idx.min(n - 1)];
            let max_v = values.last().copied().unwrap_or(0.0);
            format!("n={} p50={:.1} p95={:.1} max={:.1}", n, p50, p95, max_v)
        };
        lines.push(format!(
            "ui_input_to_paint_ms: {}",
            summarize(&self.debug.ui_input_to_paint_ms)
        ));
        lines.push(format!(
            "tab_switch_to_interactive_ms: {}",
            summarize(&self.debug.tab_switch_to_interactive_ms)
        ));
        lines.push(format!(
            "select_to_preview_ms: {}",
            summarize(&self.debug.select_to_preview_ms)
        ));
        lines.push(format!(
            "select_to_play_ms: {}",
            summarize(&self.debug.select_to_play_ms)
        ));
        lines.push(format!(
            "metadata_probe_ms: {}",
            summarize(&self.debug.metadata_probe_ms)
        ));
        lines.push(format!(
            "bg_lufs_job_ms: {}",
            summarize(&self.debug.bg_lufs_job_ms)
        ));
        lines.push(format!(
            "bg_dbfs_job_ms: {}",
            summarize(&self.debug.bg_dbfs_job_ms)
        ));
        lines.push(format!(
            "src_resample_ms: {}",
            summarize(&self.debug.src_resample_ms)
        ));
        let src_total = self.debug.src_cache_hits.saturating_add(self.debug.src_cache_misses);
        let src_hit_rate = if src_total > 0 {
            (self.debug.src_cache_hits as f64 / src_total as f64) * 100.0
        } else {
            0.0
        };
        lines.push(format!(
            "src_cache_hit_rate: {:.1}% (hits={} misses={})",
            src_hit_rate, self.debug.src_cache_hits, self.debug.src_cache_misses
        ));
        lines.push(format!(
            "plugin_scan_ms: {}",
            summarize(&self.debug.plugin_scan_ms)
        ));
        lines.push(format!(
            "plugin_probe_ms: {}",
            summarize(&self.debug.plugin_probe_ms)
        ));
        lines.push(format!(
            "plugin_preview_ms: {}",
            summarize(&self.debug.plugin_preview_ms)
        ));
        lines.push(format!(
            "plugin_apply_ms: {}",
            summarize(&self.debug.plugin_apply_ms)
        ));
        if self.debug.select_to_preview_ms.is_empty() {
            lines.push(
                "warning: select_to_preview_ms has no samples (run list selection scenario)"
                    .to_string(),
            );
        }
        if self.debug.ui_input_to_paint_ms.is_empty() {
            lines.push(
                "warning: ui_input_to_paint_ms has no samples (interact with UI and capture summary)"
                    .to_string(),
            );
        }
        if self.debug.tab_switch_to_interactive_ms.is_empty() {
            lines.push(
                "warning: tab_switch_to_interactive_ms has no samples (switch editor tabs)"
                    .to_string(),
            );
        }
        if self.debug.select_to_play_ms.is_empty() {
            lines.push(
                "warning: select_to_play_ms has no samples (run Space/AutoPlay scenario)"
                    .to_string(),
            );
        }
        if self.debug.metadata_probe_ms.is_empty() {
            lines.push(
                "warning: metadata_probe_ms has no samples (run sample-rate dependent operations)"
                    .to_string(),
            );
        }
        if self.debug.src_resample_ms.is_empty() {
            lines.push("warning: src_resample_ms has no samples (run SRC path)".to_string());
        }
        if self.debug.plugin_scan_ms.is_empty() {
            lines.push("warning: plugin_scan_ms has no samples (run plugin scan scenario)".to_string());
        }
        if self.debug.plugin_probe_ms.is_empty() {
            lines.push("warning: plugin_probe_ms has no samples (run plugin probe scenario)".to_string());
        }
        lines.push(format!(
            "autoplay_pending_count: {} stale_preview_cancel_count: {} plugin_stale_drop_count: {} plugin_worker_timeout_count: {} plugin_native_fallback_count: {}",
            self.debug.autoplay_pending_count,
            self.debug.stale_preview_cancel_count,
            self.debug.plugin_stale_drop_count,
            self.debug.plugin_worker_timeout_count,
            self.debug.plugin_native_fallback_count
        ));
        let source_count = self.external_sources.len();
        if source_count > 0 {
            let active_label = self
                .external_active_source
                .and_then(|idx| self.external_sources.get(idx))
                .map(|s| s.path.display().to_string())
                .unwrap_or_else(|| "(none)".to_string());
            let active_idx = self
                .external_active_source
                .map(|v| v.to_string())
                .unwrap_or_else(|| "none".to_string());
            lines.push(format!(
                "external_sources: {} (active: {} / {})",
                source_count, active_idx, active_label
            ));
            lines.push(format!(
                "external_rows: {} headers: {}",
                self.external_rows.len(),
                self.external_headers.len()
            ));
            if let Some(key_idx) = self.external_key_index {
                let sample_col =
                    self.external_headers
                        .iter()
                        .enumerate()
                        .find_map(|(idx, name)| {
                            if idx != key_idx {
                                Some((idx, name))
                            } else {
                                None
                            }
                        });
                if let Some((col_idx, col_name)) = sample_col {
                    let mut samples = Vec::new();
                    for row in self.external_rows.iter().take(3) {
                        let key = row.get(key_idx).map(|v| v.as_str()).unwrap_or("");
                        let val = row.get(col_idx).map(|v| v.as_str()).unwrap_or("");
                        if !key.is_empty() {
                            samples.push(format!("{key}={val}"));
                        }
                    }
                    if !samples.is_empty() {
                        lines.push(format!(
                            "external_sample {}: {}",
                            col_name,
                            samples.join(", ")
                        ));
                    }
                }
            }
            lines.push(format!(
                "external_match: {} unmatched: {} show_unmatched: {}",
                self.external_match_count,
                self.external_unmatched_count,
                self.external_show_unmatched
            ));
        } else if let Some(path) = self.external_source.as_ref() {
            lines.push(format!("external_source: {}", path.display()));
        }
        lines.join("\n")
    }

    pub(super) fn cancel_processing(&mut self) {
        self.processing = None;
        self.audio.stop();
        self.cancel_list_preview_job();
        self.list_preview_pending_path = None;
        self.list_play_pending = false;
        self.list_preview_prefetch_tx = None;
        self.list_preview_prefetch_rx = None;
        self.list_preview_prefetch_inflight.clear();
        self.list_preview_cache.clear();
        self.list_preview_cache_order.clear();
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
            action: DebugAction::SelectNext,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SelectNext,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SelectNext,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SelectNext,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::SelectNext,
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
                        self.debug_log(format!("auto: loop_xfade {}ms ({} samples)", ms, samples));
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
                            .clamp(0.0, (tab.samples_len - 1) as f32)
                            as usize;
                        let label = Self::next_marker_label(&tab.markers);
                        let entry = crate::markers::MarkerEntry { label, sample: pos };
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
                if self.files.is_empty() {
                    return;
                }
                let cur = self.selected.unwrap_or(0);
                let next = if self.selected.is_some() {
                    (cur + 1).min(self.files.len().saturating_sub(1))
                } else {
                    0
                };
                self.select_and_load(next, true);
                self.debug_log(format!("auto: select {next}"));
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
                let path = self
                    .startup
                    .cfg
                    .debug_summary_path
                    .clone()
                    .unwrap_or_else(|| self.default_debug_summary_path());
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

fn write_debug_external_merge_csvs(
    path_a: &std::path::Path,
    path_b: &std::path::Path,
    rows: usize,
    cols: usize,
    has_header: bool,
) -> Result<(), String> {
    let overlap = (rows / 2).max(1);
    write_debug_external_dummy_csv_with_offset(path_a, rows, cols, has_header, 0, "A")?;
    write_debug_external_dummy_csv_with_offset(path_b, rows, cols, has_header, overlap, "B")?;
    Ok(())
}

fn write_debug_external_dummy_csv_with_offset(
    path: &std::path::Path,
    rows: usize,
    cols: usize,
    has_header: bool,
    offset: usize,
    tag: &str,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create dir failed: {e}"))?;
    }
    let mut out = String::new();
    if has_header {
        let mut headers = Vec::with_capacity(cols);
        headers.push("Key".to_string());
        for i in 1..cols {
            headers.push(format!("Col{}", i + 1));
        }
        out.push_str(&headers.join(","));
        out.push('\n');
    }
    for i in 0..rows {
        let mut row = Vec::with_capacity(cols);
        row.push(format!("dummy_{:05}.wav", offset + i + 1));
        for c in 1..cols {
            row.push(format!("{tag}{}_{}", c + 1, i + 1));
        }
        out.push_str(&row.join(","));
        out.push('\n');
    }
    std::fs::write(path, out).map_err(|e| format!("write dummy csv failed: {e}"))?;
    Ok(())
}
