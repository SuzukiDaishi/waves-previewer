use egui::RichText;

impl crate::app::WavesPreviewer {
    /// Edit-history panel for the active editor tab: chronological list of
    /// undoable operations, the current state, and redoable operations.
    /// Clicking a row walks the existing undo/redo paths the right number
    /// of steps, so all restore semantics stay identical to Ctrl+Z/Y.
    pub(crate) fn ui_undo_history_window(&mut self, ctx: &egui::Context) {
        if !self.show_undo_history_window {
            return;
        }
        let mut open = true;
        let mut jump: Option<(bool, usize)> = None; // (redo, steps)
        egui::Window::new("Edit History")
            .open(&mut open)
            .default_width(300.0)
            .default_height(420.0)
            .vscroll(true)
            .show(ctx, |ui| {
                let tab_idx = self
                    .active_tab
                    .filter(|_| self.is_editor_workspace_active());
                let Some(tab_idx) = tab_idx else {
                    ui.label(
                        RichText::new("Open an editor tab to see its edit history.").weak(),
                    );
                    return;
                };
                let Some(tab) = self.tabs.get(tab_idx) else {
                    return;
                };
                let apply_busy = self
                    .editor_apply_state
                    .as_ref()
                    .map(|s| s.tab_id == tab.tab_id)
                    .unwrap_or(false);
                if tab.undo_stack.is_empty() && tab.redo_stack.is_empty() {
                    ui.label(RichText::new("No edits yet.").weak());
                    return;
                }
                let undo_len = tab.undo_stack.len();
                // Past operations, oldest first. Clicking one reverts it and
                // everything after it.
                for (i, state) in tab.undo_stack.iter().enumerate() {
                    let steps = undo_len - i;
                    let text = format!("{}", state.label);
                    if ui
                        .add_enabled(!apply_busy, egui::SelectableLabel::new(false, text))
                        .on_hover_text(format!("Undo {steps} step(s) back to before this edit"))
                        .clicked()
                    {
                        jump = Some((false, steps));
                    }
                }
                let _ = ui.selectable_label(true, "● Current");
                // Future operations, nearest first. Clicking one re-applies
                // up to and including it.
                for (j, state) in tab.redo_stack.iter().rev().enumerate() {
                    let text = format!("{}", state.label);
                    if ui
                        .add_enabled(
                            !apply_busy,
                            egui::SelectableLabel::new(false, RichText::new(text).weak()),
                        )
                        .on_hover_text(format!("Redo {} step(s)", j + 1))
                        .clicked()
                    {
                        jump = Some((true, j + 1));
                    }
                }
            });
        if let Some((redo, steps)) = jump {
            self.undo_history_jump(redo, steps);
        }
        self.show_undo_history_window = open;
    }

    pub(in crate::app) fn undo_history_jump(&mut self, redo: bool, steps: usize) -> usize {
        let Some(tab_idx) = self.active_tab else {
            return 0;
        };
        self.clear_preview_if_any(tab_idx);
        self.cancel_editor_apply_for_tab(tab_idx);
        let mut done = 0;
        for _ in 0..steps {
            let ok = if redo {
                self.redo_in_tab(tab_idx)
            } else {
                self.undo_in_tab(tab_idx)
            };
            if !ok {
                break;
            }
            done += 1;
        }
        if done > 0 {
            self.last_undo_scope = crate::app::types::UndoScope::Editor;
        }
        done
    }

    #[cfg(feature = "kittest")]
    pub fn test_undo_history_labels(&self) -> (Vec<String>, Vec<String>) {
        let Some(tab) = self.active_tab.and_then(|i| self.tabs.get(i)) else {
            return (Vec::new(), Vec::new());
        };
        (
            tab.undo_stack.iter().map(|s| s.label.clone()).collect(),
            tab.redo_stack.iter().map(|s| s.label.clone()).collect(),
        )
    }

    #[cfg(feature = "kittest")]
    pub fn test_undo_history_jump(&mut self, redo: bool, steps: usize) -> usize {
        self.undo_history_jump(redo, steps)
    }
}
