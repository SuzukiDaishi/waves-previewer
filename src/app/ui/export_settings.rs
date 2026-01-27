use crate::app::types::{ConflictPolicy, SaveMode, SpectrogramScale, ThemeMode, WindowFunction};
use egui::RichText;

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_export_settings_window(&mut self, ctx: &egui::Context) {
        if self.show_export_settings {
            egui::Window::new("Settings")
                .resizable(true)
                .show(ctx, |ui| {
                    ui.label("Default Save Mode:");
                    ui.horizontal(|ui| {
                        let m = self.export_cfg.save_mode;
                        if ui
                            .selectable_label(m == SaveMode::Overwrite, "Overwrite")
                            .clicked()
                        {
                            self.export_cfg.save_mode = SaveMode::Overwrite;
                        }
                        if ui
                            .selectable_label(m == SaveMode::NewFile, "New File")
                            .clicked()
                        {
                            self.export_cfg.save_mode = SaveMode::NewFile;
                        }
                    });
                    if self.export_cfg.save_mode == SaveMode::NewFile {
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.label("Destination Folder:");
                            let folder = self
                                .export_cfg
                                .dest_folder
                                .as_ref()
                                .and_then(|p| p.to_str())
                                .unwrap_or("(source folder)");
                            ui.label(RichText::new(folder).monospace());
                            if ui.button("Choose...").clicked() {
                                if let Some(d) = self.pick_folder_dialog() {
                                    self.export_cfg.dest_folder = Some(d);
                                }
                            }
                            if ui.button("Clear").clicked() {
                                self.export_cfg.dest_folder = None;
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Name Template:");
                            ui.text_edit_singleline(&mut self.export_cfg.name_template);
                        });
                        ui.horizontal(|ui| {
                            ui.label("On Conflict:");
                            let c = self.export_cfg.conflict;
                            if ui
                                .selectable_label(c == ConflictPolicy::Rename, "Rename")
                                .clicked()
                            {
                                self.export_cfg.conflict = ConflictPolicy::Rename;
                            }
                            if ui
                                .selectable_label(c == ConflictPolicy::Overwrite, "Overwrite")
                                .clicked()
                            {
                                self.export_cfg.conflict = ConflictPolicy::Overwrite;
                            }
                            if ui
                                .selectable_label(c == ConflictPolicy::Skip, "Skip")
                                .clicked()
                            {
                                self.export_cfg.conflict = ConflictPolicy::Skip;
                            }
                        });
                    } else {
                        ui.separator();
                        ui.checkbox(&mut self.export_cfg.backup_bak, ".bak backup on overwrite");
                    }
                    ui.separator();
                    ui.label("Appearance:");
                    let mut next_theme = self.theme_mode;
                    ui.horizontal(|ui| {
                        if ui
                            .selectable_label(self.theme_mode == ThemeMode::Dark, "Dark")
                            .clicked()
                        {
                            next_theme = ThemeMode::Dark;
                        }
                        if ui
                            .selectable_label(self.theme_mode == ThemeMode::Light, "Light")
                            .clicked()
                        {
                            next_theme = ThemeMode::Light;
                        }
                    });
                    if next_theme != self.theme_mode {
                        self.set_theme(ctx, next_theme);
                    }
                    ui.separator();
                    ui.label("List:");
                    let mut next_skip = self.skip_dotfiles;
                    if ui.checkbox(&mut next_skip, "Skip dotfiles (.*)").changed() {
                        self.skip_dotfiles = next_skip;
                        self.save_prefs();
                        if let Some(root) = self.root.clone() {
                            self.start_scan_folder(root);
                        } else if self.skip_dotfiles {
                            self.items.retain(|item| !Self::is_dotfile_path(&item.path));
                            self.rebuild_item_indexes();
                            self.apply_filter_from_search();
                            self.apply_sort();
                        }
                    }
                    ui.separator();
                    ui.label("List Columns:");
                    let mut next_cols = self.list_columns;
                    ui.horizontal_wrapped(|ui| {
                        ui.checkbox(&mut next_cols.edited, "Edited");
                        ui.checkbox(&mut next_cols.file, "File");
                        ui.checkbox(&mut next_cols.folder, "Folder");
                        ui.checkbox(&mut next_cols.transcript, "Transcript");
                        if self.external_visible_columns.is_empty() {
                            ui.add_enabled(
                                false,
                                egui::Checkbox::new(&mut next_cols.external, "External"),
                            );
                        } else {
                            ui.checkbox(&mut next_cols.external, "External");
                        }
                        ui.checkbox(&mut next_cols.length, "Length");
                        ui.checkbox(&mut next_cols.channels, "Ch");
                        ui.checkbox(&mut next_cols.sample_rate, "SR");
                        ui.checkbox(&mut next_cols.bits, "Bits");
                        ui.checkbox(&mut next_cols.bit_rate, "Bitrate");
                        ui.checkbox(&mut next_cols.peak, "Peak");
                        ui.checkbox(&mut next_cols.lufs, "LUFS");
                        ui.checkbox(&mut next_cols.bpm, "BPM");
                        ui.checkbox(&mut next_cols.created_at, "Created");
                        ui.checkbox(&mut next_cols.modified_at, "Modified");
                        ui.checkbox(&mut next_cols.gain, "Gain");
                        ui.checkbox(&mut next_cols.wave, "Wave");
                    });
                    let external_available = !self.external_visible_columns.is_empty();
                    let any_visible = next_cols.edited
                        || next_cols.file
                        || next_cols.folder
                        || next_cols.transcript
                        || (next_cols.external && external_available)
                        || next_cols.length
                        || next_cols.channels
                        || next_cols.sample_rate
                        || next_cols.bits
                        || next_cols.bit_rate
                        || next_cols.peak
                        || next_cols.lufs
                        || next_cols.bpm
                        || next_cols.created_at
                        || next_cols.modified_at
                        || next_cols.gain
                        || next_cols.wave;
                    if !any_visible {
                        next_cols.file = true;
                    }
                    if next_cols != self.list_columns {
                        self.list_columns = next_cols;
                        self.ensure_sort_key_visible();
                        self.apply_sort();
                    }
                    ui.separator();
                    ui.label("Spectrogram:");
                    let mut next_cfg = self.spectro_cfg.clone();
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Preset: Default").clicked() {
                            next_cfg = crate::app::types::SpectrogramConfig::default();
                        }
                        if ui.button("Preset: Ultra (Low-Freq)").clicked() {
                            next_cfg = crate::app::types::SpectrogramConfig {
                                fft_size: 4096,
                                window: WindowFunction::BlackmanHarris,
                                overlap: 0.9,
                                max_frames: 8192,
                                scale: SpectrogramScale::Log,
                                mel_scale: SpectrogramScale::Linear,
                                db_floor: -120.0,
                                max_freq_hz: 8000.0,
                                show_note_labels: false,
                            };
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label("FFT Size:");
                        egui::ComboBox::from_id_salt("spectro_fft")
                            .selected_text(format!("{}", next_cfg.fft_size))
                            .show_ui(ui, |ui| {
                                for size in [
                                    256usize, 512, 1024, 2048, 4096, 8192, 16384, 32768, 65536,
                                ] {
                                    ui.selectable_value(&mut next_cfg.fft_size, size, format!("{size}"));
                                }
                            });
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Window:");
                        egui::ComboBox::from_id_salt("spectro_window")
                            .selected_text(match next_cfg.window {
                                WindowFunction::Hann => "Hann",
                                WindowFunction::BlackmanHarris => "Blackman-Harris",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut next_cfg.window,
                                    WindowFunction::Hann,
                                    "Hann",
                                );
                                ui.selectable_value(
                                    &mut next_cfg.window,
                                    WindowFunction::BlackmanHarris,
                                    "Blackman-Harris",
                                );
                            });
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Overlap:");
                        let mut pct = next_cfg.overlap * 100.0;
                        if ui
                            .add(egui::Slider::new(&mut pct, 50.0..=95.0).suffix("%"))
                            .changed()
                        {
                            next_cfg.overlap = pct / 100.0;
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Max Frames:");
                        let mut frames = next_cfg.max_frames as i64;
                        if ui
                            .add(egui::DragValue::new(&mut frames).range(256..=8192))
                            .changed()
                        {
                            next_cfg.max_frames = frames as usize;
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Scale:");
                        egui::ComboBox::from_id_salt("spectro_scale")
                            .selected_text(match next_cfg.scale {
                                SpectrogramScale::Linear => "Linear",
                                SpectrogramScale::Log => "Log",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut next_cfg.scale,
                                    SpectrogramScale::Linear,
                                    "Linear",
                                );
                                ui.selectable_value(
                                    &mut next_cfg.scale,
                                    SpectrogramScale::Log,
                                    "Log",
                                );
                            });
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Mel Scale:");
                        egui::ComboBox::from_id_salt("spectro_mel_scale")
                            .selected_text(match next_cfg.mel_scale {
                                SpectrogramScale::Linear => "Linear",
                                SpectrogramScale::Log => "Log",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut next_cfg.mel_scale,
                                    SpectrogramScale::Linear,
                                    "Linear",
                                );
                                ui.selectable_value(
                                    &mut next_cfg.mel_scale,
                                    SpectrogramScale::Log,
                                    "Log",
                                );
                            });
                    });
                    ui.checkbox(&mut next_cfg.show_note_labels, "Show note labels (C, C#...)");
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Dynamic Range Floor (dB):");
                        let mut floor = next_cfg.db_floor;
                        if ui
                            .add(egui::Slider::new(&mut floor, -160.0..=-20.0))
                            .changed()
                        {
                            next_cfg.db_floor = floor;
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Max Frequency (Hz, 0=Nyquist):");
                        let mut max_hz = next_cfg.max_freq_hz;
                        if ui
                            .add(egui::DragValue::new(&mut max_hz).range(0.0..=192000.0).speed(100.0))
                            .changed()
                        {
                            next_cfg.max_freq_hz = max_hz;
                        }
                    });
                    if next_cfg != self.spectro_cfg {
                        self.apply_spectro_config(next_cfg);
                    }
                    ui.separator();
                    if ui.button("Close").clicked() {
                        self.show_export_settings = false;
                    }
                });
        }
    }
}
