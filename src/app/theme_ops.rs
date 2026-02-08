use std::path::PathBuf;

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, TextStyle, Visuals};

use super::types::{
    ItemBgMode, SpectrogramConfig, SpectrogramScale, SrcQuality, ThemeMode, WindowFunction,
};
use super::WavesPreviewer;

impl WavesPreviewer {
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
        if !cfg.overlap.is_finite() {
            cfg.overlap = 0.875;
        }
        cfg.overlap = cfg.overlap.clamp(0.0, 0.95);
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
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let mut plugin_paths = Vec::<PathBuf>::new();
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
            } else if let Some(rest) = line.strip_prefix("spectro_overlap=") {
                if let Ok(v) = rest.trim().parse::<f32>() {
                    self.spectro_cfg.overlap = v;
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
        Self::normalize_spectro_cfg(&mut self.spectro_cfg);
        if !plugin_paths.is_empty() {
            self.plugin_search_paths = plugin_paths;
            Self::normalize_plugin_search_paths(&mut self.plugin_search_paths);
        }
    }

    pub(super) fn save_prefs(&self) {
        let Some(path) = Self::prefs_path() else {
            return;
        };
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
spectro_overlap={:.4}\n\
spectro_max_frames={}\n\
spectro_scale={}\n\
spectro_mel_scale={}\n\
spectro_db_floor={:.1}\n\
spectro_max_hz={:.1}\n\
spectro_note_labels={}\n\
item_bg_mode={}\n\
src_quality={}\n\
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
                self.spectro_cfg.overlap,
                self.spectro_cfg.max_frames,
                scale,
                mel_scale,
                self.spectro_cfg.db_floor,
                self.spectro_cfg.max_freq_hz,
                note_labels,
                item_bg_mode,
                src_quality,
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
        for p in &self.plugin_search_paths {
            let path_text = p.to_string_lossy().replace('\n', " ");
            out.push_str("plugin_search_path=");
            out.push_str(&path_text);
            out.push('\n');
        }
        let _ = std::fs::write(path, out);
    }
}
