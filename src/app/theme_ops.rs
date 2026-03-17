use std::path::PathBuf;

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, TextStyle, Visuals};

use super::types::{
    ConflictPolicy, ExportConfig, ItemBgMode, ListColumnConfig, SaveMode, SpectrogramConfig,
    SpectrogramScale, SrcQuality, ThemeMode, TranscriptComputeTarget, TranscriptModelVariant,
    TranscriptPerfMode, WindowFunction,
};
use super::WavesPreviewer;

impl WavesPreviewer {
    pub(super) fn reset_transcript_settings_to_default(&mut self) {
        self.transcript_ai_cfg = super::types::TranscriptAiConfig::default();
        self.sanitize_transcript_ai_config();
        self.refresh_transcript_ai_status();
        self.transcript_ai_last_error = None;
        self.save_prefs();
    }

    pub(super) fn reset_settings_window_to_default(&mut self, ctx: &egui::Context) {
        self.export_cfg = ExportConfig {
            first_prompt: true,
            save_mode: SaveMode::NewFile,
            dest_folder: None,
            name_template: "{name} (gain{gain:+.1}dB)".into(),
            format_override: None,
            conflict: ConflictPolicy::Rename,
            backup_bak: true,
            export_srt: false,
        };

        let prev_skip = self.skip_dotfiles;
        self.skip_dotfiles = true;
        self.item_bg_mode = ItemBgMode::Standard;
        self.src_quality = SrcQuality::Good;
        self.list_columns = ListColumnConfig::default();
        self.zero_cross_epsilon = 1.0e-4;
        self.transcript_ai_cfg = super::types::TranscriptAiConfig::default();
        self.sanitize_transcript_ai_config();
        self.refresh_transcript_ai_status();
        self.transcript_ai_last_error = None;
        self.reset_plugin_search_paths_to_default();
        self.ensure_sort_key_visible();
        self.apply_sort();

        if self.theme_mode != ThemeMode::Dark {
            self.theme_mode = ThemeMode::Dark;
            Self::apply_theme_visuals(ctx, self.theme_mode);
        }

        self.apply_spectro_config(SpectrogramConfig::default());

        if !prev_skip && self.skip_dotfiles {
            if let Some(root) = self.root.clone() {
                self.start_scan_folder(root);
            } else {
                self.items.retain(|item| !Self::is_dotfile_path(&item.path));
                self.rebuild_item_indexes();
                self.apply_filter_from_search();
                self.apply_sort();
            }
        }
        self.save_prefs();
        self.debug_log("settings reset to defaults".to_string());
    }

    fn theme_visuals(theme: ThemeMode) -> Visuals {
        let mut visuals = match theme {
            ThemeMode::Dark => Visuals::dark(),
            ThemeMode::Light => Visuals::light(),
        };
        match theme {
            ThemeMode::Dark => {
                visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(20, 20, 23);
                visuals.widgets.inactive.bg_fill = Color32::from_rgb(28, 28, 32);
                visuals.panel_fill = Color32::from_rgb(18, 18, 20);
            }
            ThemeMode::Light => {
                visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(245, 245, 248);
                visuals.widgets.inactive.bg_fill = Color32::from_rgb(235, 235, 240);
                visuals.panel_fill = Color32::from_rgb(250, 250, 252);
            }
        }
        // Remove hover brightening to avoid sluggish tracking effect
        visuals.widgets.hovered = visuals.widgets.inactive.clone();
        visuals.widgets.active = visuals.widgets.inactive.clone();
        visuals
    }

    pub(super) fn apply_theme_visuals(ctx: &egui::Context, theme: ThemeMode) {
        ctx.set_visuals(Self::theme_visuals(theme));
    }

    pub(super) fn set_theme(&mut self, ctx: &egui::Context, theme: ThemeMode) {
        if self.theme_mode != theme {
            self.theme_mode = theme;
            Self::apply_theme_visuals(ctx, theme);
            self.save_prefs();
        }
    }

    pub(super) fn init_egui_style(ctx: &egui::Context) {
        let mut fonts = FontDefinitions::default();
        let candidates = [
            "C:/Windows/Fonts/meiryo.ttc",
            "C:/Windows/Fonts/YuGothM.ttc",
            "C:/Windows/Fonts/msgothic.ttc",
        ];
        for p in candidates {
            if let Ok(bytes) = std::fs::read(p) {
                fonts
                    .font_data
                    .insert("jp".into(), FontData::from_owned(bytes).into());
                fonts
                    .families
                    .get_mut(&FontFamily::Proportional)
                    .unwrap()
                    .insert(0, "jp".into());
                fonts
                    .families
                    .get_mut(&FontFamily::Monospace)
                    .unwrap()
                    .insert(0, "jp".into());
                break;
            }
        }
        ctx.set_fonts(fonts);

        let mut style = (*ctx.style()).clone();
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(16.0));
        style
            .text_styles
            .insert(TextStyle::Monospace, FontId::monospace(14.0));
        style.visuals = Self::theme_visuals(ThemeMode::Dark);
        ctx.set_style(style);
    }

    pub(super) fn ensure_theme_visuals(&self, ctx: &egui::Context) {
        let want_dark = self.theme_mode == ThemeMode::Dark;
        if ctx.style().visuals.dark_mode != want_dark {
            Self::apply_theme_visuals(ctx, self.theme_mode);
        }
    }

    fn prefs_path() -> Option<PathBuf> {
        let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("LOCALAPPDATA"))?;
        let mut path = PathBuf::from(base);
        path.push("NeoWaves");
        let _ = std::fs::create_dir_all(&path);
        path.push("prefs.txt");
        Some(path)
    }

    fn normalize_spectro_cfg(cfg: &mut SpectrogramConfig) {
        if !cfg.fft_size.is_power_of_two() {
            cfg.fft_size = cfg.fft_size.next_power_of_two();
        }
        cfg.fft_size = cfg.fft_size.clamp(256, 65536);
        if cfg.hop_size == 0 {
            let overlap = if cfg.overlap.is_finite() {
                cfg.overlap.clamp(0.0, 0.95)
            } else {
                0.875
            };
            cfg.hop_size = ((cfg.fft_size as f32) * (1.0 - overlap)).round().max(1.0) as usize;
        }
        let max_hop = cfg.fft_size.saturating_sub(1).max(1);
        cfg.hop_size = cfg.hop_size.clamp(1, max_hop);
        cfg.overlap = (1.0 - (cfg.hop_size as f32 / cfg.fft_size as f32)).clamp(0.0, 0.95);
        if cfg.max_frames == 0 {
            cfg.max_frames = 4096;
        }
        cfg.max_frames = cfg.max_frames.clamp(256, 8192);
        if !cfg.db_floor.is_finite() {
            cfg.db_floor = -120.0;
        }
        cfg.db_floor = cfg.db_floor.clamp(-160.0, -20.0);
        if !cfg.max_freq_hz.is_finite() || cfg.max_freq_hz < 0.0 {
            cfg.max_freq_hz = 0.0;
        }
    }

    pub(super) fn apply_spectro_config(&mut self, mut next: SpectrogramConfig) {
        Self::normalize_spectro_cfg(&mut next);
        if next == self.spectro_cfg {
            return;
        }
        self.spectro_cfg = next;
        self.save_prefs();
        self.cancel_all_spectrograms();
        self.spectro_cache.clear();
        self.spectro_cache_order.clear();
        self.spectro_cache_sizes.clear();
        self.spectro_cache_bytes = 0;
        self.spectro_inflight.clear();
        self.spectro_progress.clear();
        self.spectro_cancel.clear();
    }

    pub(super) fn load_prefs(&mut self) {
        let Some(path) = Self::prefs_path() else {
            return;
        };
        self.load_prefs_from_path(path.as_path());
    }

    pub(super) fn load_prefs_from_path(&mut self, path: &std::path::Path) {
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let mut plugin_paths = Vec::<PathBuf>::new();
        let mut spectro_hop_loaded = false;
        let mut spectro_overlap_legacy: Option<f32> = None;
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("theme=") {
                self.theme_mode = match rest {
                    "light" => ThemeMode::Light,
                    _ => ThemeMode::Dark,
                };
            } else if let Some(rest) = line.strip_prefix("skip_dotfiles=") {
                let v = matches!(rest.trim(), "1" | "true" | "yes" | "on");
                self.skip_dotfiles = v;
            } else if let Some(rest) = line.strip_prefix("zero_cross_eps=") {
                if let Ok(v) = rest.trim().parse::<f32>() {
                    if v.is_finite() {
                        self.zero_cross_epsilon = v.max(0.0);
                    }
                }
            } else if let Some(rest) = line.strip_prefix("spectro_fft=") {
                if let Ok(v) = rest.trim().parse::<usize>() {
                    self.spectro_cfg.fft_size = v;
                }
            } else if let Some(rest) = line.strip_prefix("spectro_window=") {
                self.spectro_cfg.window = match rest.trim() {
                    "hann" => WindowFunction::Hann,
                    _ => WindowFunction::BlackmanHarris,
                };
            } else if let Some(rest) = line.strip_prefix("spectro_hop=") {
                if let Ok(v) = rest.trim().parse::<usize>() {
                    self.spectro_cfg.hop_size = v.max(1);
                    spectro_hop_loaded = true;
                }
            } else if let Some(rest) = line.strip_prefix("spectro_overlap=") {
                if let Ok(v) = rest.trim().parse::<f32>() {
                    spectro_overlap_legacy = Some(v);
                }
            } else if let Some(rest) = line.strip_prefix("spectro_max_frames=") {
                if let Ok(v) = rest.trim().parse::<usize>() {
                    self.spectro_cfg.max_frames = v;
                }
            } else if let Some(rest) = line.strip_prefix("spectro_scale=") {
                self.spectro_cfg.scale = match rest.trim() {
                    "linear" => SpectrogramScale::Linear,
                    _ => SpectrogramScale::Log,
                };
            } else if let Some(rest) = line.strip_prefix("spectro_mel_scale=") {
                self.spectro_cfg.mel_scale = match rest.trim() {
                    "log" => SpectrogramScale::Log,
                    _ => SpectrogramScale::Linear,
                };
            } else if let Some(rest) = line.strip_prefix("spectro_db_floor=") {
                if let Ok(v) = rest.trim().parse::<f32>() {
                    self.spectro_cfg.db_floor = v;
                }
            } else if let Some(rest) = line.strip_prefix("spectro_max_hz=") {
                if let Ok(v) = rest.trim().parse::<f32>() {
                    self.spectro_cfg.max_freq_hz = v;
                }
            } else if let Some(rest) = line.strip_prefix("spectro_note_labels=") {
                let v = matches!(rest.trim(), "1" | "true" | "yes" | "on");
                self.spectro_cfg.show_note_labels = v;
            } else if let Some(rest) = line.strip_prefix("item_bg_mode=") {
                self.item_bg_mode = match rest.trim().to_ascii_lowercase().as_str() {
                    "dbfs" => ItemBgMode::Dbfs,
                    "lufs" => ItemBgMode::Lufs,
                    _ => ItemBgMode::Standard,
                };
            } else if let Some(rest) = line.strip_prefix("src_quality=") {
                self.src_quality = match rest.trim().to_ascii_lowercase().as_str() {
                    "fast" => SrcQuality::Fast,
                    "best" => SrcQuality::Best,
                    _ => SrcQuality::Good,
                };
            } else if let Some(rest) = line.strip_prefix("audio_output_device=") {
                let v = rest.trim();
                self.audio_output_device_name = if v.is_empty() {
                    None
                } else {
                    Some(v.to_string())
                };
            } else if let Some(rest) = line.strip_prefix("transcript_ai_opt_in=") {
                self.transcript_ai_opt_in = matches!(rest.trim(), "1" | "true" | "yes" | "on");
            } else if let Some(rest) = line.strip_prefix("transcript_language=") {
                let value = rest.trim();
                if !value.is_empty() {
                    self.transcript_ai_cfg.language = value.to_string();
                }
            } else if let Some(rest) = line.strip_prefix("transcript_task=") {
                let value = rest.trim();
                if !value.is_empty() {
                    self.transcript_ai_cfg.task = value.to_string();
                }
            } else if let Some(rest) = line.strip_prefix("transcript_max_new_tokens=") {
                if let Ok(v) = rest.trim().parse::<usize>() {
                    self.transcript_ai_cfg.max_new_tokens = v.clamp(1, 512);
                }
            } else if let Some(rest) = line.strip_prefix("transcript_perf_mode=") {
                self.transcript_ai_cfg.perf_mode = match rest.trim().to_ascii_lowercase().as_str() {
                    "balanced" => TranscriptPerfMode::Balanced,
                    "boost" => TranscriptPerfMode::Boost,
                    _ => TranscriptPerfMode::Stable,
                };
            } else if let Some(rest) = line.strip_prefix("transcript_model_variant=") {
                self.transcript_ai_cfg.model_variant =
                    match rest.trim().to_ascii_lowercase().as_str() {
                        "fp16" => TranscriptModelVariant::Fp16,
                        "quantized" => TranscriptModelVariant::Quantized,
                        _ => TranscriptModelVariant::Auto,
                    };
            } else if let Some(rest) = line.strip_prefix("transcript_overwrite_existing_srt=") {
                self.transcript_ai_cfg.overwrite_existing_srt =
                    matches!(rest.trim(), "1" | "true" | "yes" | "on");
            } else if let Some(rest) = line.strip_prefix("transcript_omit_language_token=") {
                self.transcript_ai_cfg.omit_language_token =
                    matches!(rest.trim(), "1" | "true" | "yes" | "on");
            } else if let Some(rest) = line.strip_prefix("transcript_omit_notimestamps_token=") {
                self.transcript_ai_cfg.omit_notimestamps_token =
                    matches!(rest.trim(), "1" | "true" | "yes" | "on");
            } else if let Some(rest) = line.strip_prefix("transcript_vad_enabled=") {
                self.transcript_ai_cfg.vad_enabled =
                    matches!(rest.trim(), "1" | "true" | "yes" | "on");
            } else if let Some(rest) = line.strip_prefix("transcript_vad_model_path=") {
                let raw = rest.trim().trim_matches('"');
                if raw.is_empty() {
                    self.transcript_ai_cfg.vad_model_path = None;
                } else {
                    self.transcript_ai_cfg.vad_model_path = Some(PathBuf::from(raw));
                }
            } else if let Some(rest) = line.strip_prefix("transcript_vad_threshold=") {
                if let Ok(v) = rest.trim().parse::<f32>() {
                    if v.is_finite() {
                        self.transcript_ai_cfg.vad_threshold = v.clamp(0.01, 0.99);
                    }
                }
            } else if let Some(rest) = line.strip_prefix("transcript_vad_min_speech_ms=") {
                if let Ok(v) = rest.trim().parse::<usize>() {
                    self.transcript_ai_cfg.vad_min_speech_ms = v.clamp(10, 10_000);
                }
            } else if let Some(rest) = line.strip_prefix("transcript_vad_min_silence_ms=") {
                if let Ok(v) = rest.trim().parse::<usize>() {
                    self.transcript_ai_cfg.vad_min_silence_ms = v.clamp(10, 10_000);
                }
            } else if let Some(rest) = line.strip_prefix("transcript_vad_speech_pad_ms=") {
                if let Ok(v) = rest.trim().parse::<usize>() {
                    self.transcript_ai_cfg.vad_speech_pad_ms = v.clamp(0, 5_000);
                }
            } else if let Some(rest) = line.strip_prefix("transcript_max_window_ms=") {
                if let Ok(v) = rest.trim().parse::<usize>() {
                    self.transcript_ai_cfg.max_window_ms = v.clamp(1_000, 30_000);
                }
            } else if let Some(rest) = line.strip_prefix("transcript_no_speech_threshold=") {
                let t = rest.trim();
                if t.is_empty() {
                    self.transcript_ai_cfg.no_speech_threshold = None;
                } else if let Ok(v) = t.parse::<f32>() {
                    if v.is_finite() {
                        self.transcript_ai_cfg.no_speech_threshold = Some(v.clamp(0.0, 1.0));
                    }
                }
            } else if let Some(rest) = line.strip_prefix("transcript_logprob_threshold=") {
                let t = rest.trim();
                if t.is_empty() {
                    self.transcript_ai_cfg.logprob_threshold = None;
                } else if let Ok(v) = t.parse::<f32>() {
                    if v.is_finite() {
                        self.transcript_ai_cfg.logprob_threshold = Some(v.clamp(-10.0, 0.0));
                    }
                }
            } else if let Some(rest) = line.strip_prefix("transcript_compute_target=") {
                self.transcript_ai_cfg.compute_target =
                    match rest.trim().to_ascii_lowercase().as_str() {
                        "cpu" => TranscriptComputeTarget::Cpu,
                        "gpu" => TranscriptComputeTarget::Gpu,
                        "npu" => TranscriptComputeTarget::Npu,
                        _ => TranscriptComputeTarget::Auto,
                    };
            } else if let Some(rest) = line.strip_prefix("transcript_dml_device_id=") {
                if let Ok(v) = rest.trim().parse::<i32>() {
                    self.transcript_ai_cfg.dml_device_id = v.clamp(0, 16);
                }
            } else if let Some(rest) = line.strip_prefix("transcript_cpu_intra_threads=") {
                if let Ok(v) = rest.trim().parse::<usize>() {
                    self.transcript_ai_cfg.cpu_intra_threads = v.min(64);
                }
            } else if let Some(rest) = line.strip_prefix("export_srt=") {
                self.export_cfg.export_srt = matches!(rest.trim(), "1" | "true" | "yes" | "on");
            } else if let Some(rest) = line.strip_prefix("plugin_search_path=") {
                let raw = rest.trim().trim_matches('"');
                if !raw.is_empty() {
                    plugin_paths.push(PathBuf::from(raw));
                }
            } else if let Some(rest) = line.strip_prefix("zoo_enabled=") {
                self.zoo_enabled = matches!(rest.trim(), "1" | "true" | "yes" | "on");
            } else if let Some(rest) = line.strip_prefix("zoo_walk_enabled=") {
                self.zoo_walk_enabled = matches!(rest.trim(), "1" | "true" | "yes" | "on");
            } else if let Some(rest) = line.strip_prefix("zoo_voice_enabled=") {
                self.zoo_voice_enabled = matches!(rest.trim(), "1" | "true" | "yes" | "on");
            } else if let Some(rest) = line.strip_prefix("zoo_use_bpm=") {
                self.zoo_use_bpm = matches!(rest.trim(), "1" | "true" | "yes" | "on");
            } else if let Some(rest) = line.strip_prefix("zoo_gif_path=") {
                let raw = rest.trim().trim_matches('"');
                if !raw.is_empty() {
                    self.zoo_gif_path = Some(PathBuf::from(raw));
                }
            } else if let Some(rest) = line.strip_prefix("zoo_voice_path=") {
                let raw = rest.trim().trim_matches('"');
                if !raw.is_empty() {
                    self.zoo_voice_path = Some(PathBuf::from(raw));
                }
            } else if let Some(rest) = line.strip_prefix("zoo_scale=") {
                if let Ok(v) = rest.trim().parse::<f32>() {
                    if v.is_finite() {
                        self.zoo_scale = v.clamp(0.25, 2.5);
                    }
                }
            } else if let Some(rest) = line.strip_prefix("zoo_opacity=") {
                if let Ok(v) = rest.trim().parse::<f32>() {
                    if v.is_finite() {
                        self.zoo_opacity = v.clamp(0.3, 1.0);
                    }
                }
            } else if let Some(rest) = line.strip_prefix("zoo_speed=") {
                if let Ok(v) = rest.trim().parse::<f32>() {
                    if v.is_finite() {
                        self.zoo_speed = v.clamp(40.0, 360.0);
                    }
                }
            } else if let Some(rest) = line.strip_prefix("zoo_flip_manual=") {
                self.zoo_flip_manual = matches!(rest.trim(), "1" | "true" | "yes" | "on");
            }
        }
        if !spectro_hop_loaded {
            if let Some(overlap) = spectro_overlap_legacy {
                let overlap = if overlap.is_finite() {
                    overlap.clamp(0.0, 0.95)
                } else {
                    0.875
                };
                self.spectro_cfg.hop_size = ((self.spectro_cfg.fft_size.max(2) as f32)
                    * (1.0 - overlap))
                    .round()
                    .max(1.0) as usize;
            }
        }
        Self::normalize_spectro_cfg(&mut self.spectro_cfg);
        if !plugin_paths.is_empty() {
            self.plugin_search_paths = plugin_paths;
            Self::normalize_plugin_search_paths(&mut self.plugin_search_paths);
        }
        self.sanitize_transcript_ai_config();
    }

    pub(super) fn save_prefs(&self) {
        let Some(path) = Self::prefs_path() else {
            return;
        };
        self.save_prefs_to_path(path.as_path());
    }

    pub(super) fn save_prefs_to_path(&self, path: &std::path::Path) {
        let theme = match self.theme_mode {
            ThemeMode::Dark => "dark",
            ThemeMode::Light => "light",
        };
        let skip = if self.skip_dotfiles { "1" } else { "0" };
        let window = match self.spectro_cfg.window {
            WindowFunction::Hann => "hann",
            WindowFunction::BlackmanHarris => "blackman_harris",
        };
        let scale = match self.spectro_cfg.scale {
            SpectrogramScale::Linear => "linear",
            SpectrogramScale::Log => "log",
        };
        let mel_scale = match self.spectro_cfg.mel_scale {
            SpectrogramScale::Linear => "linear",
            SpectrogramScale::Log => "log",
        };
        let note_labels = if self.spectro_cfg.show_note_labels {
            "1"
        } else {
            "0"
        };
        let item_bg_mode = match self.item_bg_mode {
            ItemBgMode::Standard => "standard",
            ItemBgMode::Dbfs => "dbfs",
            ItemBgMode::Lufs => "lufs",
        };
        let src_quality = match self.src_quality {
            SrcQuality::Fast => "fast",
            SrcQuality::Good => "good",
            SrcQuality::Best => "best",
        };
        let audio_output_device = self.audio_output_device_name.as_deref().unwrap_or("");
        let transcript_ai_opt_in = if self.transcript_ai_opt_in { "1" } else { "0" };
        let transcript_overwrite_existing_srt = if self.transcript_ai_cfg.overwrite_existing_srt {
            "1"
        } else {
            "0"
        };
        let transcript_perf_mode = match self.transcript_ai_cfg.perf_mode {
            TranscriptPerfMode::Stable => "stable",
            TranscriptPerfMode::Balanced => "balanced",
            TranscriptPerfMode::Boost => "boost",
        };
        let transcript_model_variant = match self.transcript_ai_cfg.model_variant {
            TranscriptModelVariant::Auto => "auto",
            TranscriptModelVariant::Fp16 => "fp16",
            TranscriptModelVariant::Quantized => "quantized",
        };
        let transcript_omit_language_token = if self.transcript_ai_cfg.omit_language_token {
            "1"
        } else {
            "0"
        };
        let transcript_omit_notimestamps_token = if self.transcript_ai_cfg.omit_notimestamps_token {
            "1"
        } else {
            "0"
        };
        let transcript_vad_enabled = if self.transcript_ai_cfg.vad_enabled {
            "1"
        } else {
            "0"
        };
        let transcript_compute_target = match self.transcript_ai_cfg.compute_target {
            TranscriptComputeTarget::Auto => "auto",
            TranscriptComputeTarget::Cpu => "cpu",
            TranscriptComputeTarget::Gpu => "gpu",
            TranscriptComputeTarget::Npu => "npu",
        };
        let export_srt = if self.export_cfg.export_srt { "1" } else { "0" };
        let zoo_enabled = if self.zoo_enabled { "1" } else { "0" };
        let zoo_walk_enabled = if self.zoo_walk_enabled { "1" } else { "0" };
        let zoo_voice_enabled = if self.zoo_voice_enabled { "1" } else { "0" };
        let zoo_use_bpm = if self.zoo_use_bpm { "1" } else { "0" };
        let zoo_flip_manual = if self.zoo_flip_manual { "1" } else { "0" };
        let mut out = format!(
            "theme={}\nskip_dotfiles={}\n\
zero_cross_eps={:.6}\n\
spectro_fft={}\n\
spectro_window={}\n\
spectro_hop={}\n\
spectro_overlap={:.4}\n\
spectro_max_frames={}\n\
spectro_scale={}\n\
spectro_mel_scale={}\n\
spectro_db_floor={:.1}\n\
spectro_max_hz={:.1}\n\
spectro_note_labels={}\n\
item_bg_mode={}\n\
src_quality={}\n\
audio_output_device={}\n\
transcript_ai_opt_in={}\n\
transcript_language={}\n\
transcript_task={}\n\
transcript_max_new_tokens={}\n\
transcript_perf_mode={}\n\
transcript_model_variant={}\n\
transcript_overwrite_existing_srt={}\n\
transcript_omit_language_token={}\n\
transcript_omit_notimestamps_token={}\n\
transcript_vad_enabled={}\n\
transcript_vad_threshold={:.3}\n\
transcript_vad_min_speech_ms={}\n\
transcript_vad_min_silence_ms={}\n\
transcript_vad_speech_pad_ms={}\n\
transcript_max_window_ms={}\n\
transcript_no_speech_threshold={}\n\
transcript_logprob_threshold={}\n\
transcript_compute_target={}\n\
transcript_dml_device_id={}\n\
transcript_cpu_intra_threads={}\n\
export_srt={}\n\
zoo_enabled={}\n\
zoo_walk_enabled={}\n\
zoo_voice_enabled={}\n\
zoo_use_bpm={}\n\
zoo_scale={:.3}\n\
zoo_opacity={:.3}\n\
zoo_speed={:.1}\n\
zoo_flip_manual={}\n",
            theme,
            skip,
            self.zero_cross_epsilon,
            self.spectro_cfg.fft_size,
            window,
            self.spectro_cfg.hop_size,
            self.spectro_cfg.overlap,
            self.spectro_cfg.max_frames,
            scale,
            mel_scale,
            self.spectro_cfg.db_floor,
            self.spectro_cfg.max_freq_hz,
            note_labels,
            item_bg_mode,
            src_quality,
            audio_output_device,
            transcript_ai_opt_in,
            self.transcript_ai_cfg.language,
            self.transcript_ai_cfg.task,
            self.transcript_ai_cfg.max_new_tokens,
            transcript_perf_mode,
            transcript_model_variant,
            transcript_overwrite_existing_srt,
            transcript_omit_language_token,
            transcript_omit_notimestamps_token,
            transcript_vad_enabled,
            self.transcript_ai_cfg.vad_threshold,
            self.transcript_ai_cfg.vad_min_speech_ms,
            self.transcript_ai_cfg.vad_min_silence_ms,
            self.transcript_ai_cfg.vad_speech_pad_ms,
            self.transcript_ai_cfg.max_window_ms,
            self.transcript_ai_cfg
                .no_speech_threshold
                .map(|v| format!("{v:.3}"))
                .unwrap_or_default(),
            self.transcript_ai_cfg
                .logprob_threshold
                .map(|v| format!("{v:.3}"))
                .unwrap_or_default(),
            transcript_compute_target,
            self.transcript_ai_cfg.dml_device_id,
            self.transcript_ai_cfg.cpu_intra_threads,
            export_srt,
            zoo_enabled,
            zoo_walk_enabled,
            zoo_voice_enabled,
            zoo_use_bpm,
            self.zoo_scale,
            self.zoo_opacity,
            self.zoo_speed,
            zoo_flip_manual
        );
        if let Some(path) = &self.zoo_gif_path {
            out.push_str("zoo_gif_path=");
            out.push_str(&path.to_string_lossy().replace('\n', " "));
            out.push('\n');
        }
        if let Some(path) = &self.zoo_voice_path {
            out.push_str("zoo_voice_path=");
            out.push_str(&path.to_string_lossy().replace('\n', " "));
            out.push('\n');
        }
        if let Some(path) = &self.transcript_ai_cfg.vad_model_path {
            out.push_str("transcript_vad_model_path=");
            out.push_str(&path.to_string_lossy().replace('\n', " "));
            out.push('\n');
        }
        for p in &self.plugin_search_paths {
            let path_text = p.to_string_lossy().replace('\n', " ");
            out.push_str("plugin_search_path=");
            out.push_str(&path_text);
            out.push('\n');
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, out);
    }
}
