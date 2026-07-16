use std::path::PathBuf;

use std::sync::Arc;

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, TextStyle, Visuals};

use super::types::{
    ConflictPolicy, EditorHorizontalZoomAnchorMode, EditorPauseResumeMode, ExportConfig,
    ItemBgMode, ListColumnConfig, SaveMode, SpectrogramConfig, SpectrogramScale, SrcQuality,
    ThemeMode, TranscriptComputeTarget, TranscriptModelVariant, TranscriptPerfMode, WindowFunction,
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

    pub(super) fn reset_zoo_settings_to_default(&mut self) {
        self.zoo_enabled = false;
        self.zoo_walk_enabled = true;
        self.zoo_voice_enabled = false;
        self.zoo_use_bpm = true;
        self.zoo_gif_path = None;
        self.zoo_voice_path = None;
        self.zoo_scale = 0.5;
        self.zoo_opacity = 1.0;
        self.zoo_speed = 140.0;
        self.zoo_flip_manual = false;
        self.zoo_anim_clock = 0.0;
        self.zoo_pos_x = 0.0;
        self.zoo_dir = 1.0;
        self.zoo_last_tick = std::time::Instant::now();
        self.zoo_squish_until = None;
        self.zoo_last_error = None;
        self.zoo_voice_cache_path = None;
        self.zoo_voice_cache = None;
        self.reload_zoo_gif_frames();
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
            codec: Default::default(),
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
        self.reset_zoo_settings_to_default();
        self.reset_plugin_search_paths_to_default();
        self.ensure_sort_key_visible();
        self.request_sort();

        if self.theme_mode != ThemeMode::Dark {
            self.theme_mode = ThemeMode::Dark;
            Self::apply_theme_visuals(ctx, self.theme_mode);
        }

        self.apply_spectro_config(SpectrogramConfig::default());

        if !prev_skip && self.skip_dotfiles {
            if let Some(root) = self.root.clone() {
                self.start_scan_folder(root);
            } else {
                let skip_dotfiles = self.skip_dotfiles;
                self.items.retain(|item| {
                    !Self::is_internal_temp_cache_path(&item.path)
                        && (!skip_dotfiles || !Self::is_dotfile_path(&item.path))
                });
                self.rebuild_item_indexes();
                self.refresh_filter_then_sort();
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
        // Write the visuals into BOTH of egui's per-theme styles: the app
        // manages its own theme mode, so the two egui styles must stay
        // identical or an OS dark/light switch would swap in stale visuals.
        let visuals = Self::theme_visuals(theme);
        ctx.all_styles_mut(|style| style.visuals = visuals.clone());
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

        // バンドルフォント: 常に使える日本語フォント (fallback)
        // NotoSansJP-Regular.otf はビルド時に埋め込まれる
        fonts.font_data.insert(
            "noto_sans_jp".into(),
            Arc::new(FontData::from_static(include_bytes!(
                "../../assets/fonts/NotoSansJP-Regular.otf"
            ))),
        );

        // システムフォント候補: 見つかればバンドルより高品質になる
        // Windows → Meiryo / Yu Gothic、macOS → Hiragino Sans、Linux → Noto Sans CJK
        let system_candidates: &[&str] = &[
            // Windows
            "C:/Windows/Fonts/meiryo.ttc",
            "C:/Windows/Fonts/YuGothM.ttc",
            "C:/Windows/Fonts/YuGothR.ttc",
            "C:/Windows/Fonts/msgothic.ttc",
            // macOS
            "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            // Linux (Noto / IPAex / Takao)
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJKjp-Regular.otf",
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/ipafont-gothic/ipag.ttf",
            "/usr/share/fonts/truetype/takao-gothic/TakaoGothic.ttf",
        ];
        let mut system_font_loaded = false;
        for path in system_candidates {
            if let Ok(bytes) = std::fs::read(path) {
                fonts
                    .font_data
                    .insert("jp_system".into(), FontData::from_owned(bytes).into());
                system_font_loaded = true;
                break;
            }
        }

        // フォント優先順位: システム > バンドル > egui デフォルト
        // 日本語 UI なので日本語フォントを先頭に配置する
        for family in [&FontFamily::Proportional, &FontFamily::Monospace] {
            let list = fonts.families.get_mut(family).unwrap();
            if system_font_loaded {
                list.insert(0, "noto_sans_jp".into());
                list.insert(0, "jp_system".into()); // システムフォントが最優先
            } else {
                list.insert(0, "noto_sans_jp".into()); // バンドルが最優先
            }
        }

        ctx.set_fonts(fonts);

        // egui keeps separate dark/light styles and, with the default
        // ThemePreference::System, swaps the active one when the OS reports
        // a theme change. Patch BOTH styles so an OS dark/light switch can
        // never swap in an unpatched style (default text sizes + debug
        // heuristics re-enabled -> the list flashing red).
        let visuals = Self::theme_visuals(ThemeMode::Dark);
        ctx.all_styles_mut(|style| {
            style
                .text_styles
                .insert(TextStyle::Body, FontId::proportional(16.0));
            style
                .text_styles
                .insert(TextStyle::Monospace, FontId::monospace(14.0));
            style.visuals = visuals.clone();
            #[cfg(debug_assertions)]
            {
                // The virtualized file-list table reuses on-screen row rects for
                // different rows after a `scroll_to_row` jump that lands on an
                // exact row-height multiple (e.g. PageDown/PageUp). This debug-only
                // heuristic then flags every visible row as a false-positive
                // "Id changed for this rect" for one frame, flashing a red border
                // around the whole list. `warn_on_id_clash` (real duplicate-Id
                // detection) stays enabled.
                style.debug.warn_if_rect_changes_id = false;
            }
        });
        // DPI スケーリングは native_pixels_per_point × zoom_factor で一元管理
        // 明示的に 1.0 を設定しておくことで widget 個別スケールとの混在を防ぐ
        ctx.set_zoom_factor(1.0);
    }

    pub(super) fn ensure_theme_visuals(&self, ctx: &egui::Context) {
        let want_dark = self.theme_mode == ThemeMode::Dark;
        if ctx.global_style().visuals.dark_mode != want_dark {
            Self::apply_theme_visuals(ctx, self.theme_mode);
        }
    }

    fn prefs_path() -> Option<PathBuf> {
        // Kittest harnesses build a real `WavesPreviewer` and must stay isolated from
        // the developer's actual saved preferences (e.g. `auto_play_list_nav`) — both
        // for read and write — so tests behave identically across machines.
        if cfg!(feature = "kittest") {
            return None;
        }
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
        self.reset_all_feature_analysis_state();
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
        let mut recent_sessions = Vec::<PathBuf>::new();
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
            } else if let Some(rest) = line.strip_prefix("editor_invert_wave_zoom_wheel=") {
                self.invert_wave_zoom_wheel = matches!(rest.trim(), "1" | "true" | "yes" | "on");
            } else if let Some(rest) = line.strip_prefix("editor_invert_shift_wheel_pan=") {
                self.invert_shift_wheel_pan = matches!(rest.trim(), "1" | "true" | "yes" | "on");
            } else if let Some(rest) = line.strip_prefix("editor_horizontal_zoom_anchor=") {
                self.horizontal_zoom_anchor_mode = match rest.trim().to_ascii_lowercase().as_str() {
                    "playhead" => EditorHorizontalZoomAnchorMode::Playhead,
                    _ => EditorHorizontalZoomAnchorMode::Pointer,
                };
            } else if let Some(rest) = line.strip_prefix("editor_pause_resume_mode=") {
                self.editor_pause_resume_mode = match rest.trim().to_ascii_lowercase().as_str() {
                    "continue_from_pause" => EditorPauseResumeMode::ContinueFromPause,
                    _ => EditorPauseResumeMode::ReturnToLastStart,
                };
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
            } else if let Some(rest) = line.strip_prefix("world_f0_method=") {
                self.world_f0_method = match rest.trim() {
                    "harvest" => crate::app::types::WorldF0Method::Harvest,
                    _ => crate::app::types::WorldF0Method::Dio,
                };
            } else if let Some(rest) = line.strip_prefix("spectro_db_ref=") {
                self.spectro_cfg.db_ref = match rest.trim() {
                    "max" => crate::app::types::SpectrogramDbRef::MaxNormalized,
                    _ => crate::app::types::SpectrogramDbRef::Absolute,
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
            } else if let Some(rest) = line.strip_prefix("auto_play_list_nav=") {
                self.auto_play_list_nav = matches!(rest.trim(), "1" | "true" | "yes" | "on");
            } else if let Some(rest) = line.strip_prefix("list_click_audition=") {
                self.list_click_audition = matches!(rest.trim(), "1" | "true" | "yes" | "on");
            } else if let Some(rest) = line.strip_prefix("list_col_widths=") {
                self.list_col_widths.clear();
                for part in rest.split(',') {
                    let Some((key, w)) = part.split_once(':') else {
                        continue;
                    };
                    if let Ok(w) = w.trim().parse::<f32>() {
                        if w.is_finite() && w >= 10.0 && !key.trim().is_empty() {
                            self.list_col_widths.insert(key.trim().to_string(), w);
                        }
                    }
                }
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
            } else if let Some(rest) = line.strip_prefix("export_mp3_kbps=") {
                if let Ok(v) = rest.trim().parse::<u32>() {
                    self.export_cfg.codec.mp3_bitrate_kbps = v.clamp(32, 320);
                }
            } else if let Some(rest) = line.strip_prefix("export_aac_kbps=") {
                if let Ok(v) = rest.trim().parse::<u32>() {
                    self.export_cfg.codec.aac_bitrate_kbps = v.clamp(32, 320);
                }
            } else if let Some(rest) = line.strip_prefix("export_ogg_quality=") {
                if let Ok(v) = rest.trim().parse::<f32>() {
                    self.export_cfg.codec.ogg_quality = v.clamp(-0.2, 1.0);
                }
            } else if let Some(rest) = line.strip_prefix("export_dither=") {
                self.export_cfg.codec.dither_16bit =
                    matches!(rest.trim(), "1" | "true" | "yes" | "on");
            } else if let Some(rest) = line.strip_prefix("recent_session=") {
                let raw = rest.trim().trim_matches('"');
                if !raw.is_empty() {
                    recent_sessions.push(PathBuf::from(raw));
                }
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
        self.set_recent_sessions_from_prefs(recent_sessions);
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
        let auto_play_list_nav = if self.auto_play_list_nav { "1" } else { "0" };
        let list_click_audition = if self.list_click_audition { "1" } else { "0" };
        let list_col_widths = self
            .list_col_widths
            .iter()
            .map(|(k, w)| format!("{k}:{w:.1}"))
            .collect::<Vec<_>>()
            .join(",");
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
        let invert_wave_zoom_wheel = if self.invert_wave_zoom_wheel {
            "1"
        } else {
            "0"
        };
        let invert_shift_wheel_pan = if self.invert_shift_wheel_pan {
            "1"
        } else {
            "0"
        };
        let horizontal_zoom_anchor = match self.horizontal_zoom_anchor_mode {
            EditorHorizontalZoomAnchorMode::Pointer => "pointer",
            EditorHorizontalZoomAnchorMode::Playhead => "playhead",
        };
        let editor_pause_resume_mode = match self.editor_pause_resume_mode {
            EditorPauseResumeMode::ReturnToLastStart => "return_to_last_start",
            EditorPauseResumeMode::ContinueFromPause => "continue_from_pause",
        };
        let mut out = format!(
            "theme={}\nskip_dotfiles={}\n\
zero_cross_eps={:.6}\n\
editor_invert_wave_zoom_wheel={}\n\
editor_invert_shift_wheel_pan={}\n\
editor_horizontal_zoom_anchor={}\n\
editor_pause_resume_mode={}\n\
spectro_fft={}\n\
spectro_window={}\n\
spectro_hop={}\n\
spectro_overlap={:.4}\n\
spectro_max_frames={}\n\
spectro_scale={}\n\
spectro_mel_scale={}\n\
spectro_db_floor={:.1}\n\
spectro_db_ref={}\n\
world_f0_method={}\n\
spectro_max_hz={:.1}\n\
spectro_note_labels={}\n\
item_bg_mode={}\n\
src_quality={}\n\
audio_output_device={}\n\
auto_play_list_nav={}\n\
list_click_audition={}\n\
list_col_widths={}\n\
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
export_mp3_kbps={}\n\
export_aac_kbps={}\n\
export_ogg_quality={:.2}\n\
export_dither={}\n\
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
            invert_wave_zoom_wheel,
            invert_shift_wheel_pan,
            horizontal_zoom_anchor,
            editor_pause_resume_mode,
            self.spectro_cfg.fft_size,
            window,
            self.spectro_cfg.hop_size,
            self.spectro_cfg.overlap,
            self.spectro_cfg.max_frames,
            scale,
            mel_scale,
            self.spectro_cfg.db_floor,
            match self.spectro_cfg.db_ref {
                crate::app::types::SpectrogramDbRef::Absolute => "absolute",
                crate::app::types::SpectrogramDbRef::MaxNormalized => "max",
            },
            match self.world_f0_method {
                crate::app::types::WorldF0Method::Dio => "dio",
                crate::app::types::WorldF0Method::Harvest => "harvest",
            },
            self.spectro_cfg.max_freq_hz,
            note_labels,
            item_bg_mode,
            src_quality,
            audio_output_device,
            auto_play_list_nav,
            list_click_audition,
            list_col_widths,
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
            self.export_cfg.codec.mp3_bitrate_kbps,
            self.export_cfg.codec.aac_bitrate_kbps,
            self.export_cfg.codec.ogg_quality,
            if self.export_cfg.codec.dither_16bit { "1" } else { "0" },
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
        for p in self.recent_session_paths_for_menu() {
            let path_text = p.to_string_lossy().replace('\n', " ");
            out.push_str("recent_session=");
            out.push_str(&path_text);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "neowaves_recent_unit_{tag}_{}_{}_{}",
            std::process::id(),
            now_ms,
            seq
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn touch(path: &std::path::Path) {
        std::fs::write(path, "session placeholder").expect("touch session");
    }

    #[test]
    fn recent_session_prefs_roundtrip_filters_dedupes_and_limits() {
        let dir = temp_dir("prefs_roundtrip");
        let first = dir.join("first.nwsess");
        let second = dir.join("second.nwsess");
        let third = dir.join("third.nwsess");
        let fourth = dir.join("fourth.nwsess");
        let legacy = dir.join("legacy.nwproj");
        let missing = dir.join("missing.nwsess");
        for path in [&first, &second, &third, &fourth, &legacy] {
            touch(path);
        }
        // Extra distinct sessions beyond the limit so truncation is actually
        // exercised (RECENT_SESSION_LIMIT is 10).
        let extras: Vec<std::path::PathBuf> = (5..=11)
            .map(|i| {
                let p = dir.join(format!("extra{i}.nwsess"));
                touch(&p);
                p
            })
            .collect();
        let prefs = dir.join("prefs.txt");

        let mut app =
            WavesPreviewer::new_headless(crate::StartupConfig::default()).expect("headless app");
        let mut input = vec![
            first.clone(),
            second.clone(),
            second.clone(),
            legacy,
            missing,
            third.clone(),
            fourth.clone(),
        ];
        input.extend(extras.iter().cloned());
        app.set_recent_sessions_from_prefs(input);
        app.save_prefs_to_path(&prefs);

        let mut loaded =
            WavesPreviewer::new_headless(crate::StartupConfig::default()).expect("headless app");
        loaded.set_recent_sessions_from_prefs(Vec::new());
        loaded.load_prefs_from_path(&prefs);

        let mut expected = vec![
            std::fs::canonicalize(first).expect("first canonical"),
            std::fs::canonicalize(second).expect("second canonical"),
            std::fs::canonicalize(third).expect("third canonical"),
            std::fs::canonicalize(fourth).expect("fourth canonical"),
        ];
        expected.extend(
            extras
                .iter()
                .take(6)
                .map(|p| std::fs::canonicalize(p).expect("extra canonical")),
        );
        assert_eq!(loaded.recent_session_paths_for_menu(), expected);
        assert_eq!(expected.len(), 10, "limit should cap the list at 10");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recent_session_insert_moves_existing_to_front_without_duplicates() {
        let dir = temp_dir("insert_order");
        let first = dir.join("first.nwsess");
        let second = dir.join("second.nwsess");
        let third = dir.join("third.nwsess");
        let fourth = dir.join("fourth.nwsess");
        for path in [&first, &second, &third, &fourth] {
            touch(path);
        }

        let mut app =
            WavesPreviewer::new_headless(crate::StartupConfig::default()).expect("headless app");
        app.set_recent_sessions_from_prefs(Vec::new());
        assert!(app.insert_recent_session_path(&first));
        assert!(app.insert_recent_session_path(&second));
        assert!(app.insert_recent_session_path(&third));
        assert!(app.insert_recent_session_path(&second));
        assert!(app.insert_recent_session_path(&fourth));

        // Under the 10-entry limit nothing is evicted; "second" moves to
        // front on its second insert instead of duplicating.
        let expected = vec![
            std::fs::canonicalize(fourth).expect("fourth canonical"),
            std::fs::canonicalize(second).expect("second canonical"),
            std::fs::canonicalize(third).expect("third canonical"),
            std::fs::canonicalize(first).expect("first canonical"),
        ];
        assert_eq!(app.recent_session_paths_for_menu(), expected);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recent_session_insert_evicts_oldest_past_limit() {
        let dir = temp_dir("insert_limit");
        let mut app =
            WavesPreviewer::new_headless(crate::StartupConfig::default()).expect("headless app");
        app.set_recent_sessions_from_prefs(Vec::new());
        let paths: Vec<std::path::PathBuf> = (0..12)
            .map(|i| {
                let p = dir.join(format!("session{i}.nwsess"));
                touch(&p);
                p
            })
            .collect();
        for path in &paths {
            assert!(app.insert_recent_session_path(path));
        }
        let menu = app.recent_session_paths_for_menu();
        assert_eq!(menu.len(), 10, "list should cap at RECENT_SESSION_LIMIT");
        // Most-recently-inserted first; the two oldest (session0, session1) are evicted.
        assert_eq!(
            menu[0],
            std::fs::canonicalize(&paths[11]).expect("canonical")
        );
        assert!(!menu.contains(&std::fs::canonicalize(&paths[0]).expect("canonical")));
        assert!(!menu.contains(&std::fs::canonicalize(&paths[1]).expect("canonical")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn col_width_prefs_roundtrip() {
        let dir = temp_dir("col_widths");
        let prefs = dir.join("prefs.txt");
        let mut app =
            WavesPreviewer::new_headless(crate::StartupConfig::default()).expect("headless app");
        app.list_col_widths.insert("file".to_string(), 314.5);
        app.list_col_widths.insert("wave".to_string(), 220.0);
        app.list_click_audition = false;
        app.save_prefs_to_path(&prefs);

        let mut loaded =
            WavesPreviewer::new_headless(crate::StartupConfig::default()).expect("headless app");
        loaded.load_prefs_from_path(&prefs);
        assert_eq!(loaded.list_col_widths.get("file").copied(), Some(314.5));
        assert_eq!(loaded.list_col_widths.get("wave").copied(), Some(220.0));
        assert!(!loaded.list_click_audition);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn col_width_prefs_reject_bogus_values() {
        let dir = temp_dir("col_widths_bogus");
        let prefs = dir.join("prefs.txt");
        std::fs::write(
            &prefs,
            "list_col_widths=file:nan,gain:5.0,wave:220.0,:60.0,broken\n",
        )
        .expect("write prefs");
        let mut loaded =
            WavesPreviewer::new_headless(crate::StartupConfig::default()).expect("headless app");
        loaded.load_prefs_from_path(&prefs);
        assert_eq!(loaded.list_col_widths.get("wave").copied(), Some(220.0));
        assert!(!loaded.list_col_widths.contains_key("file"));
        assert!(!loaded.list_col_widths.contains_key("gain"));
        assert_eq!(loaded.list_col_widths.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
