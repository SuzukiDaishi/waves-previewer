//! The channel layout window: which speaker each channel of a file feeds.
//! Opened from the mini meter's right-click menu. See
//! `app::channel_layout_ops` and `docs/MULTICHANNEL_SPEC.md`.

use std::path::PathBuf;

use egui::RichText;

use crate::app::channel_layout_ops::ChannelLayoutEditor;
use crate::app::WavesPreviewer;
use crate::audio_channels::{presets_for, SpeakerPos};

enum LayoutAction {
    None,
    ApplyToFile,
    SaveDefault,
    ResetFile,
}

/// A speaker as the menu shows it: its name in this layout, and its key.
fn speaker_text(pos: SpeakerPos, layout: &[Option<SpeakerPos>]) -> String {
    format!("{}  ({})", pos.label(layout), pos.key())
}

impl WavesPreviewer {
    /// The settings row under the output device: which speakers its
    /// outputs feed (a device with more than two).
    pub(in crate::app) fn ui_output_speakers(&mut self, ui: &mut egui::Ui) {
        let outputs = self.audio.output_channels();
        if outputs <= 2 {
            return;
        }
        let device = self.audio.output_device_name().map(str::to_string);
        let current = device
            .as_ref()
            .and_then(|device| self.output_speakers.get(device))
            .cloned();
        let mut choice = current.clone();
        ui.horizontal(|ui| {
            ui.label(format!("Speakers ({outputs} outputs):"));
            let shown = choice
                .as_ref()
                .map(|c| c.preset.clone())
                .unwrap_or_else(|| "Standard".to_string());
            egui::ComboBox::from_id_salt("output_speakers_preset")
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(choice.is_none(), "Standard").clicked() {
                        choice = None;
                    }
                    for preset in crate::audio_channels::PRESETS.iter().filter(|preset| {
                        preset.speakers.len() >= 2 && preset.speakers.len() <= outputs
                    }) {
                        let on = choice.as_ref().is_some_and(|c| c.preset == preset.name);
                        if ui.selectable_label(on, preset.name).clicked() {
                            choice = Some(crate::app::channel_layout_ops::OutputSpeakers {
                                preset: preset.name.to_string(),
                                first: 1,
                            });
                        }
                    }
                });
            if let Some(c) = choice.as_mut() {
                let width = crate::audio_channels::PRESETS
                    .iter()
                    .find(|p| p.name == c.preset)
                    .map_or(1, |p| p.speakers.len());
                ui.label("from output");
                ui.add(egui::DragValue::new(&mut c.first).range(1..=(outputs + 1 - width).max(1)));
            }
        })
        .response
        .on_hover_text(
            "Which speaker each output feeds. Files are routed to them by speaker, \
             whatever order their channels are in.",
        );
        if choice != current {
            self.set_output_speakers(choice);
        }
    }

    /// Open the layout window on `path`, starting from its current layout.
    pub(in crate::app) fn open_channel_layout_editor(&mut self, path: PathBuf, channels: usize) {
        let channels = channels.max(1);
        let draft = self
            .channel_layout_for(&path, channels)
            .unwrap_or_else(|| vec![None; channels]);
        self.channel_layout_editor = Some(ChannelLayoutEditor { path, draft });
    }

    pub(in crate::app) fn ui_channel_layout_window(&mut self, ctx: &egui::Context) {
        // Asked for from the mini meter's menu, which cannot reach `self`.
        if let Some(tab) = self.active_tab.and_then(|idx| self.tabs.get_mut(idx)) {
            if std::mem::take(&mut tab.mini_meter.layout_editor_requested) {
                let (path, channels) = (tab.path.clone(), tab.ch_samples.len());
                self.open_channel_layout_editor(path, channels);
            }
        }
        let Some(mut editor) = self.channel_layout_editor.take() else {
            return;
        };
        let channels = editor.draft.len();
        let origin = self
            .channel_layout_resolved(&editor.path, channels)
            .map(|(_, origin)| origin);
        let has_override = self.channel_layout_overrides.contains_key(&editor.path);
        let name = editor
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut open = true;
        let mut action = LayoutAction::None;
        let scroll_target = self.begin_floating_scroll_surface("channel_layout_window");
        let scroll_guard = self.pointer_scroll_input_guard(scroll_target, ctx);
        let shown = egui::Window::new("Channel layout")
            .open(&mut open)
            .resizable(false)
            .default_width(380.0)
            .show(ctx, |ui| {
                ui.label(RichText::new(format!("{name} \u{2014} {channels} channels")).strong());
                if let Some(origin) = origin {
                    ui.label(RichText::new(format!("Now: {}", origin.describe())).weak());
                }
                ui.horizontal(|ui| {
                    ui.label("Preset");
                    egui::ComboBox::from_id_salt("channel_layout_preset")
                        .selected_text("Choose\u{2026}")
                        .width(200.0)
                        .show_ui(ui, |ui| {
                            for preset in presets_for(channels) {
                                if ui.selectable_label(false, preset.name).clicked() {
                                    editor.draft =
                                        preset.speakers.iter().copied().map(Some).collect();
                                }
                            }
                        });
                });
                ui.separator();
                let snapshot = editor.draft.clone();
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        egui::Grid::new("channel_layout_grid")
                            .num_columns(2)
                            .striped(true)
                            .show(ui, |ui| {
                                for (ch, slot) in editor.draft.iter_mut().enumerate() {
                                    ui.label(format!("Ch {}", ch + 1));
                                    let text = slot.map_or_else(
                                        || "\u{2014}".to_string(),
                                        |pos| speaker_text(pos, &snapshot),
                                    );
                                    egui::ComboBox::from_id_salt(("channel_layout_ch", ch))
                                        .selected_text(text)
                                        .width(170.0)
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                slot,
                                                None,
                                                "\u{2014} (no speaker)",
                                            );
                                            for pos in SpeakerPos::ALL {
                                                ui.selectable_value(
                                                    slot,
                                                    Some(pos),
                                                    speaker_text(pos, &snapshot),
                                                );
                                            }
                                        });
                                    ui.end_row();
                                }
                            });
                    });
                // Two channels on one speaker both play there: say so.
                let doubled: Vec<&str> = SpeakerPos::ALL
                    .iter()
                    .filter(|pos| {
                        editor
                            .draft
                            .iter()
                            .filter(|slot| **slot == Some(**pos))
                            .count()
                            > 1
                    })
                    .map(|pos| pos.label(&editor.draft))
                    .collect();
                if !doubled.is_empty() {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!("On more than one channel: {}", doubled.join(", ")),
                    );
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .button("Apply to this file")
                        .on_hover_text("Stored in the session with this file.")
                        .clicked()
                    {
                        action = LayoutAction::ApplyToFile;
                    }
                    if ui
                        .button(format!("Default for {channels} channels"))
                        .on_hover_text(
                            "Stored in your preferences. A file with a channel mask in its \
                             header, or a layout of its own, keeps that.",
                        )
                        .clicked()
                    {
                        action = LayoutAction::SaveDefault;
                    }
                    if ui
                        .add_enabled(has_override, egui::Button::new("Reset this file"))
                        .on_hover_text("Forget this file's own layout.")
                        .clicked()
                    {
                        action = LayoutAction::ResetFile;
                    }
                });
            });
        drop(scroll_guard);
        if let Some(shown) = shown.as_ref() {
            self.register_scroll_surface(scroll_target, &shown.response);
        }
        let done = match action {
            LayoutAction::ApplyToFile => {
                self.set_channel_layout_override(&editor.path, Some(editor.draft.clone()));
                true
            }
            LayoutAction::SaveDefault => {
                self.set_channel_layout_default(editor.draft.clone());
                true
            }
            LayoutAction::ResetFile => {
                self.set_channel_layout_override(&editor.path, None);
                editor.draft = self
                    .channel_layout_for(&editor.path, channels)
                    .unwrap_or_else(|| vec![None; channels]);
                false
            }
            LayoutAction::None => false,
        };
        if open && !done {
            self.channel_layout_editor = Some(editor);
        }
    }
}
