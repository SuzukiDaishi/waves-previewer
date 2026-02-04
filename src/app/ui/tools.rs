use egui::RichText;
use std::collections::BTreeMap;

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_tool_palette_window(&mut self, ctx: &egui::Context) {
        if !self.show_tool_palette {
            return;
        }
        let mut open = self.show_tool_palette;
        egui::Window::new("Command Palette")
            .open(&mut open)
            .resizable(true)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                if let Some(path) = Self::tools_config_path() {
                    ui.label(format!("Config: {}", path.display()));
                }
                ui.horizontal(|ui| {
                    if ui.button("Reload").clicked() {
                        self.load_tools_config();
                        self.tool_config_error = None;
                    }
                    let has_cfg = Self::tools_config_path()
                        .map(|p| p.exists())
                        .unwrap_or(false);
                    if ui
                        .add_enabled(!has_cfg, egui::Button::new("Create Sample"))
                        .clicked()
                    {
                        match self.write_sample_tools_config() {
                            Ok(_) => {
                                self.load_tools_config();
                                self.tool_config_error = None;
                            }
                            Err(err) => self.tool_config_error = Some(err),
                        }
                    }
                    if ui.button("Clear Log").clicked() {
                        self.tool_log.clear();
                    }
                });
                if let Some(err) = self.tool_config_error.as_ref() {
                    ui.colored_label(egui::Color32::LIGHT_RED, err);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Filter");
                    ui.text_edit_singleline(&mut self.tool_search);
                });

                let filter = self.tool_search.trim().to_ascii_lowercase();
                let selected_paths = self.selected_paths();
                let has_selection = !selected_paths.is_empty();
                let tools = self.tool_defs.clone();
                let mut grouped: BTreeMap<String, Vec<crate::app::tooling::ToolDef>> =
                    BTreeMap::new();
                for tool in tools {
                    if !filter.is_empty() {
                        let name = tool.name.to_ascii_lowercase();
                        let desc = tool
                            .description
                            .as_ref()
                            .map(|d| d.to_ascii_lowercase())
                            .unwrap_or_default();
                        if !name.contains(&filter) && !desc.contains(&filter) {
                            continue;
                        }
                    }
                    let group = tool
                        .group
                        .clone()
                        .filter(|g| !g.trim().is_empty())
                        .unwrap_or_else(|| "General".to_string());
                    grouped.entry(group).or_default().push(tool);
                }
                if self.tool_selected.is_none() {
                    if let Some((_, list)) = grouped.iter().next() {
                        if let Some(first) = list.first() {
                            self.tool_selected = Some(first.name.clone());
                        }
                    }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.set_min_width(260.0);
                        egui::ScrollArea::vertical()
                            .max_height(360.0)
                            .show(ui, |ui| {
                                for (group, list) in grouped.iter_mut() {
                                    egui::CollapsingHeader::new(group).default_open(true).show(
                                        ui,
                                        |ui| {
                                            for tool in list {
                                                let selected =
                                                    self.tool_selected.as_ref() == Some(&tool.name);
                                                let label = RichText::new(&tool.name).strong();
                                                if ui.selectable_label(selected, label).clicked() {
                                                    self.tool_selected = Some(tool.name.clone());
                                                }
                                            }
                                        },
                                    );
                                }
                            });
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_min_width(320.0);
                        let mut selected_tool: Option<crate::app::tooling::ToolDef> = None;
                        for list in grouped.values() {
                            for tool in list {
                                if self.tool_selected.as_ref() == Some(&tool.name) {
                                    selected_tool = Some(tool.clone());
                                    break;
                                }
                            }
                            if selected_tool.is_some() {
                                break;
                            }
                        }
                        if let Some(tool) = selected_tool {
                            ui.label(RichText::new(&tool.name).strong());
                            if let Some(desc) = tool.description.as_ref() {
                                ui.label(desc);
                            }
                            ui.separator();
                            let per_file = tool.per_file.unwrap_or(true);
                            ui.label(if per_file {
                                "Scope: per selected file"
                            } else {
                                "Scope: run once"
                            });
                            let mut run_args: Option<String> = None;
                            let default_args = tool.args.clone().unwrap_or_default();
                            {
                                let args = self
                                    .tool_args_overrides
                                    .entry(tool.name.clone())
                                    .or_insert(default_args);
                                ui.horizontal(|ui| {
                                    ui.label("Args");
                                    ui.text_edit_singleline(args);
                                });
                                ui.separator();
                                ui.label("Command");
                                ui.monospace(&tool.command);
                                let preview_target = selected_paths.first().cloned();
                                let preview_args = args.clone();
                                let preview_cmd = crate::app::WavesPreviewer::expand_tool_command(
                                    &tool.command,
                                    preview_target.as_deref(),
                                    &preview_args,
                                );
                                ui.label("Preview");
                                ui.monospace(preview_cmd);
                                ui.separator();
                                let can_run = if per_file { has_selection } else { true };
                                if ui.add_enabled(can_run, egui::Button::new("Run")).clicked() {
                                    run_args = Some(args.clone());
                                }
                                if per_file && !has_selection {
                                    ui.label(
                                        RichText::new("Select files to run this tool.").weak(),
                                    );
                                }
                            }
                            if let Some(run_args) = run_args {
                                self.enqueue_tool_runs(&tool, &selected_paths, &run_args);
                            }
                        } else {
                            ui.label("No tools available.");
                        }
                    });
                });
                if !self.tool_queue.is_empty() {
                    ui.separator();
                    ui.label(format!("Queue: {}", self.tool_queue.len()));
                }
                if self.tool_worker_busy {
                    ui.label("Running...");
                }
                ui.separator();
                ui.label(RichText::new("History").strong());
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .show(ui, |ui| {
                        for entry in &self.tool_log {
                            let status = if entry.ok { "OK" } else { "FAIL" };
                            let target = entry
                                .path
                                .as_ref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| "(none)".to_string());
                            ui.label(format!("[{}] {} - {}", status, entry.tool_name, target));
                        }
                    });
            });
        self.show_tool_palette = open;
    }

    pub(in crate::app) fn ui_tool_confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(job) = self.pending_tool_confirm.clone() else {
            return;
        };
        let mut open = true;
        egui::Window::new("Confirm Tool Command")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!("Tool: {}", job.tool.name));
                let target = job
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(none)".to_string());
                ui.label(format!("Target: {}", target));
                ui.separator();
                ui.label("Command:");
                ui.monospace(&job.command);
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Run").clicked() {
                        self.pending_tool_confirm = None;
                        self.start_tool_job(job.clone());
                    }
                    if ui.button("Cancel").clicked() {
                        self.pending_tool_confirm = None;
                    }
                });
            });
        if !open {
            self.pending_tool_confirm = None;
        }
    }
}
