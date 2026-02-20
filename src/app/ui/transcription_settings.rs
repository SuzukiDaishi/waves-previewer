use crate::app::types::{TranscriptComputeTarget, TranscriptModelVariant, TranscriptPerfMode};
use egui::RichText;

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_transcription_settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_transcription_settings {
            return;
        }
        let mut open = self.show_transcription_settings;
        egui::Window::new("AI > Transcription")
            .open(&mut open)
            .resizable(true)
            .default_size(egui::vec2(760.0, 620.0))
            .show(ctx, |ui| {
                let max_h = (ctx.content_rect().height() * 0.78).max(320.0);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height(max_h)
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            if self.transcript_ai_has_model() {
                                ui.label("Model: ready");
                                if ui
                                    .add_enabled(
                                        self.transcript_ai_can_uninstall(),
                                        egui::Button::new("Uninstall Model..."),
                                    )
                                    .clicked()
                                {
                                    self.uninstall_transcript_model_cache();
                                }
                            } else if self.transcript_ai_model_dir.is_some() {
                                ui.label("Model: selected variant missing");
                                if ui.button("Download Model...").clicked() {
                                    self.queue_transcript_model_download();
                                }
                            } else {
                                ui.label("Model: not installed");
                                if ui.button("Download Model...").clicked() {
                                    self.queue_transcript_model_download();
                                }
                            }
                        });
                        if let Some(dir) = &self.transcript_ai_model_dir {
                            ui.add_sized(
                                [ui.available_width(), 0.0],
                                egui::Label::new(
                                    RichText::new(format!("Model dir: {}", dir.display())).small(),
                                )
                                .wrap(),
                            );
                        }
                        if let Some(err) = &self.transcript_ai_last_error {
                            ui.label(
                                RichText::new(err).color(egui::Color32::from_rgb(220, 120, 120)),
                            );
                        }
                        ui.separator();

                        let mut transcript_cfg_dirty = false;
                        let languages = self.transcript_language_options();
                        let tasks = self.transcript_task_options();
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Language:");
                            egui::ComboBox::from_id_salt("transcript_ai_language_combo")
                                .selected_text(self.transcript_ai_cfg.language.as_str())
                                .show_ui(ui, |ui| {
                                    for lang in &languages {
                                        if ui
                                            .selectable_value(
                                                &mut self.transcript_ai_cfg.language,
                                                lang.clone(),
                                                lang,
                                            )
                                            .changed()
                                        {
                                            transcript_cfg_dirty = true;
                                        }
                                    }
                                });
                            ui.label("Task:");
                            egui::ComboBox::from_id_salt("transcript_ai_task_combo")
                                .selected_text(self.transcript_ai_cfg.task.as_str())
                                .show_ui(ui, |ui| {
                                    for task in &tasks {
                                        if ui
                                            .selectable_value(
                                                &mut self.transcript_ai_cfg.task,
                                                task.clone(),
                                                task,
                                            )
                                            .changed()
                                        {
                                            transcript_cfg_dirty = true;
                                        }
                                    }
                                });
                        });

                        ui.horizontal_wrapped(|ui| {
                            ui.label("Max New Tokens:");
                            let mut max_new_tokens =
                                self.transcript_ai_cfg.max_new_tokens.clamp(1, 512) as u32;
                            if ui
                                .add(
                                    egui::DragValue::new(&mut max_new_tokens)
                                        .range(1..=512)
                                        .speed(1.0),
                                )
                                .changed()
                            {
                                self.transcript_ai_cfg.max_new_tokens = max_new_tokens as usize;
                                transcript_cfg_dirty = true;
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Mode:");
                            let mut mode = self.transcript_ai_cfg.perf_mode;
                            ui.selectable_value(&mut mode, TranscriptPerfMode::Stable, "Stable");
                            ui.selectable_value(
                                &mut mode,
                                TranscriptPerfMode::Balanced,
                                "Balanced",
                            );
                            ui.selectable_value(&mut mode, TranscriptPerfMode::Boost, "Boost");
                            if mode != self.transcript_ai_cfg.perf_mode {
                                self.transcript_ai_cfg.perf_mode = mode;
                                transcript_cfg_dirty = true;
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Model:");
                            let mut variant = self.transcript_ai_cfg.model_variant;
                            ui.selectable_value(&mut variant, TranscriptModelVariant::Auto, "Auto");
                            ui.selectable_value(&mut variant, TranscriptModelVariant::Fp16, "FP16");
                            ui.selectable_value(
                                &mut variant,
                                TranscriptModelVariant::Quantized,
                                "Quantized",
                            );
                            if variant != self.transcript_ai_cfg.model_variant {
                                self.transcript_ai_cfg.model_variant = variant;
                                transcript_cfg_dirty = true;
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            transcript_cfg_dirty |= ui
                                .checkbox(
                                    &mut self.transcript_ai_cfg.overwrite_existing_srt,
                                    "Overwrite existing .srt",
                                )
                                .changed();
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Compute:");
                            let mut next_target = self.transcript_ai_cfg.compute_target;
                            ui.selectable_value(
                                &mut next_target,
                                TranscriptComputeTarget::Auto,
                                "Auto",
                            );
                            ui.selectable_value(
                                &mut next_target,
                                TranscriptComputeTarget::Cpu,
                                "CPU",
                            );
                            ui.selectable_value(
                                &mut next_target,
                                TranscriptComputeTarget::Gpu,
                                "GPU",
                            );
                            ui.selectable_value(
                                &mut next_target,
                                TranscriptComputeTarget::Npu,
                                "NPU",
                            );
                            if next_target != self.transcript_ai_cfg.compute_target {
                                self.transcript_ai_cfg.compute_target = next_target;
                                transcript_cfg_dirty = true;
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            let mut cpu_threads = self.transcript_ai_cfg.cpu_intra_threads as u32;
                            if ui
                                .add(
                                    egui::DragValue::new(&mut cpu_threads)
                                        .range(0..=64)
                                        .prefix("CPU threads (0=auto): "),
                                )
                                .changed()
                            {
                                self.transcript_ai_cfg.cpu_intra_threads = cpu_threads as usize;
                                transcript_cfg_dirty = true;
                            }
                            let mut dml_device = self.transcript_ai_cfg.dml_device_id;
                            if ui
                                .add(
                                    egui::DragValue::new(&mut dml_device)
                                        .range(0..=16)
                                        .prefix("DML device: "),
                                )
                                .changed()
                            {
                                self.transcript_ai_cfg.dml_device_id = dml_device;
                                transcript_cfg_dirty = true;
                            }
                        });
                        ui.label(
                            RichText::new(format!(
                                "Runtime fallback: requested accelerator -> CPU (workers: {})",
                                self.transcript_estimated_parallel_workers()
                            ))
                            .small()
                            .weak(),
                        );
                        ui.horizontal_wrapped(|ui| {
                            transcript_cfg_dirty |= ui
                                .checkbox(
                                    &mut self.transcript_ai_cfg.omit_language_token,
                                    "Omit language token",
                                )
                                .changed();
                            transcript_cfg_dirty |= ui
                                .checkbox(
                                    &mut self.transcript_ai_cfg.omit_notimestamps_token,
                                    "Allow timestamps token",
                                )
                                .changed();
                        });

                        ui.separator();
                        ui.label("Silero VAD:");
                        ui.horizontal_wrapped(|ui| {
                            transcript_cfg_dirty |= ui
                                .checkbox(
                                    &mut self.transcript_ai_cfg.vad_enabled,
                                    "Enable VAD split",
                                )
                                .changed();
                            let detected = self.transcript_ai_effective_vad_model_path();
                            if detected.is_some() {
                                ui.label("Model: ready");
                            } else {
                                ui.label("Model: not found (falls back to fixed 30s chunks)");
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("Choose VAD model...").clicked() {
                                if let Some(path) = rfd::FileDialog::new()
                                    .add_filter("ONNX", &["onnx"])
                                    .pick_file()
                                {
                                    self.transcript_ai_cfg.vad_model_path = Some(path);
                                    transcript_cfg_dirty = true;
                                }
                            }
                            if ui.button("Clear VAD path").clicked() {
                                self.transcript_ai_cfg.vad_model_path = None;
                                transcript_cfg_dirty = true;
                            }
                        });
                        if let Some(path) = self.transcript_ai_effective_vad_model_path() {
                            ui.add_sized(
                                [ui.available_width(), 0.0],
                                egui::Label::new(
                                    RichText::new(format!("VAD model: {}", path.display())).small(),
                                )
                                .wrap(),
                            );
                        }
                        ui.horizontal_wrapped(|ui| {
                            let mut threshold =
                                self.transcript_ai_cfg.vad_threshold.clamp(0.01, 0.99);
                            if ui
                                .add(
                                    egui::DragValue::new(&mut threshold)
                                        .range(0.01..=0.99)
                                        .speed(0.01)
                                        .fixed_decimals(2),
                                )
                                .changed()
                            {
                                self.transcript_ai_cfg.vad_threshold = threshold;
                                transcript_cfg_dirty = true;
                            }
                            ui.label("threshold");
                        });
                        ui.horizontal_wrapped(|ui| {
                            let mut min_speech = self.transcript_ai_cfg.vad_min_speech_ms as u32;
                            let mut min_silence = self.transcript_ai_cfg.vad_min_silence_ms as u32;
                            let mut speech_pad = self.transcript_ai_cfg.vad_speech_pad_ms as u32;
                            let mut max_window = self.transcript_ai_cfg.max_window_ms as u32;
                            if ui
                                .add(
                                    egui::DragValue::new(&mut min_speech)
                                        .range(10..=10_000)
                                        .prefix("min speech(ms): "),
                                )
                                .changed()
                            {
                                self.transcript_ai_cfg.vad_min_speech_ms = min_speech as usize;
                                transcript_cfg_dirty = true;
                            }
                            if ui
                                .add(
                                    egui::DragValue::new(&mut min_silence)
                                        .range(10..=10_000)
                                        .prefix("min silence(ms): "),
                                )
                                .changed()
                            {
                                self.transcript_ai_cfg.vad_min_silence_ms = min_silence as usize;
                                transcript_cfg_dirty = true;
                            }
                            if ui
                                .add(
                                    egui::DragValue::new(&mut speech_pad)
                                        .range(0..=5_000)
                                        .prefix("pad(ms): "),
                                )
                                .changed()
                            {
                                self.transcript_ai_cfg.vad_speech_pad_ms = speech_pad as usize;
                                transcript_cfg_dirty = true;
                            }
                            if ui
                                .add(
                                    egui::DragValue::new(&mut max_window)
                                        .range(1_000..=30_000)
                                        .prefix("max window(ms): "),
                                )
                                .changed()
                            {
                                self.transcript_ai_cfg.max_window_ms = max_window as usize;
                                transcript_cfg_dirty = true;
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            let mut use_no_speech =
                                self.transcript_ai_cfg.no_speech_threshold.is_some();
                            if ui
                                .checkbox(&mut use_no_speech, "Use no_speech_threshold")
                                .changed()
                            {
                                if use_no_speech {
                                    self.transcript_ai_cfg
                                        .no_speech_threshold
                                        .get_or_insert(0.6);
                                } else {
                                    self.transcript_ai_cfg.no_speech_threshold = None;
                                }
                                transcript_cfg_dirty = true;
                            }
                            if let Some(mut v) = self.transcript_ai_cfg.no_speech_threshold {
                                if ui
                                    .add(
                                        egui::DragValue::new(&mut v)
                                            .range(0.0..=1.0)
                                            .speed(0.01)
                                            .fixed_decimals(2),
                                    )
                                    .changed()
                                {
                                    self.transcript_ai_cfg.no_speech_threshold =
                                        Some(v.clamp(0.0, 1.0));
                                    transcript_cfg_dirty = true;
                                }
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            let mut use_logprob =
                                self.transcript_ai_cfg.logprob_threshold.is_some();
                            if ui
                                .checkbox(&mut use_logprob, "Use logprob_threshold")
                                .changed()
                            {
                                if use_logprob {
                                    self.transcript_ai_cfg.logprob_threshold.get_or_insert(-1.0);
                                } else {
                                    self.transcript_ai_cfg.logprob_threshold = None;
                                }
                                transcript_cfg_dirty = true;
                            }
                            if let Some(mut v) = self.transcript_ai_cfg.logprob_threshold {
                                if ui
                                    .add(
                                        egui::DragValue::new(&mut v)
                                            .range(-10.0..=0.0)
                                            .speed(0.05)
                                            .fixed_decimals(2),
                                    )
                                    .changed()
                                {
                                    self.transcript_ai_cfg.logprob_threshold =
                                        Some(v.clamp(-10.0, 0.0));
                                    transcript_cfg_dirty = true;
                                }
                            }
                        });

                        if transcript_cfg_dirty {
                            self.sanitize_transcript_ai_config();
                            self.refresh_transcript_ai_status();
                            self.save_prefs();
                        }
                    });
            });
        self.show_transcription_settings = open;
    }
}
