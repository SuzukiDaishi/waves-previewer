use egui::Sense;

use crate::app::input_focus::UiSurface;
use crate::app::WavesPreviewer;

use super::{ListInteractionState, ListViewMetrics};

/// How many times `key` was pressed this frame, auto-repeat included.
///
/// One step per frame is not the same thing as one step per key press. A
/// frame that ran long -- a sort landing, a preview decode, a folder of
/// metadata arriving -- carries every repeat the keyboard produced while it
/// was busy, and acting on one of them while dropping the rest is what makes
/// a held arrow stall on a row. Stepping by the count keeps the selection at
/// the speed the keyboard is going, and costs one `select_and_load` for the
/// row actually landed on rather than one per row passed over.
fn key_presses(ctx: &egui::Context, key: egui::Key) -> usize {
    // `Modifiers::NONE` matches logically here, which lets Shift and Alt
    // through -- exactly what keeps Shift+Arrow range selection arriving as
    // an arrow.
    let consumed = ctx.input_mut(|i| i.count_and_consume_key(egui::Modifiers::NONE, key));
    // Presses another widget consumed first never reach `consume_key`, and
    // the list still owns the arrows in that case (a focused topbar
    // DragValue hands them straight back). The raw log is where those are
    // still visible.
    let raw = ctx.input(|i| {
        i.raw
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    egui::Event::Key {
                        key: pressed,
                        pressed: true,
                        ..
                    } if *pressed == key
                )
            })
            .count()
    });
    consumed.max(raw)
}

/// Whether a chord was typed this frame.
///
/// A chord is how focus moves on purpose without the pointer -- Ctrl+F is the
/// search box -- and egui's own arrow navigation never fires for a modified
/// arrow, so one of these is reason enough to leave a focus change alone.
/// Read from the raw log: a shortcut that already fired has consumed its
/// event out of `events` by the time the list draws.
fn modified_key_pressed(ctx: &egui::Context) -> bool {
    ctx.input(|i| {
        i.raw.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Key {
                    pressed: true,
                    modifiers,
                    ..
                } if modifiers.any()
            )
        })
    })
}

/// Where `steps` rows from `cur` lands, clamped to the list.
fn step_row(cur: usize, steps: isize, last: usize) -> usize {
    if steps >= 0 {
        cur.saturating_add(steps as usize).min(last)
    } else {
        cur.saturating_sub(steps.unsigned_abs())
    }
}

impl WavesPreviewer {
    pub(super) fn handle_list_focus_and_keyboard(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        metrics: &ListViewMetrics,
    ) -> ListInteractionState {
        let list_focus_id = crate::app::WavesPreviewer::list_focus_id();
        // egui resolves its own arrow-key focus navigation in `end_pass`,
        // after this ran, so a steal is only visible on the frame after the
        // arrow that caused it. Nothing but a pointer press or a chord the
        // user typed should move focus out of a list they are arrowing
        // through -- and where it lands is usually a text field, which owns
        // every key the list needs from then on. Take it back; one key press
        // is lost, where the alternative is a list that stops answering until
        // it is clicked.
        if std::mem::take(&mut self.list_arrow_focus_guard)
            && self.is_list_workspace_active()
            && !ctx.input(|i| i.pointer.any_pressed())
            && !modified_key_pressed(ctx)
            && ctx.memory(|m| m.focused().is_some_and(|id| id != list_focus_id))
        {
            ctx.memory_mut(|m| {
                m.stop_text_input();
                m.request_focus(list_focus_id);
            });
            self.search_has_focus = false;
            self.list_has_focus = true;
            ctx.request_repaint();
        }
        let list_focus_now = ctx.memory(|m| m.has_focus(list_focus_id));
        let focused_id = ctx.memory(|m| m.focused());
        let search_focused =
            ctx.memory(|m| m.has_focus(crate::app::WavesPreviewer::search_box_id()));
        let has_non_list_focus = focused_id.is_some() && focused_id != Some(list_focus_id);
        // Another surface (a dialog, the editor, the graph) owns the keys.
        let list_owns_keys = self.surface_keys_allowed(UiSurface::List);
        let allow_focus_reclaim = list_owns_keys && !search_focused && !has_non_list_focus;
        let focus_resp = ui.interact(metrics.list_rect, list_focus_id, Sense::click());
        if self.list_has_focus && !list_focus_now && allow_focus_reclaim {
            Self::focus_list_widget(ctx);
        }
        let _ = focus_resp;

        let mut list_has_focus = list_focus_now || self.list_has_focus;
        if !list_has_focus
            && self.is_list_workspace_active()
            && self.selected.is_some()
            && !self.search_has_focus
            && allow_focus_reclaim
        {
            Self::focus_list_widget(ctx);
            list_has_focus = true;
            self.list_has_focus = true;
        }

        let mut key_moved = false;
        // The list only acts on keys while the user is actually in it: no
        // caret is live anywhere (a dialog's text field, an inline rename, the
        // search box, a topbar DragValue in edit mode -- `list_owns_keys`
        // folds in `text_edit_focused()`) and no other surface owns the
        // frame's keys.
        //
        // Deliberately NOT gated on `has_non_list_focus`: a plain topbar
        // button takes egui focus when clicked, and the list's arrows have to
        // survive that. Keys the caret does want are handled by the widget
        // itself -- `topbar/transport.rs:133` consumes a focused DragValue's
        // arrows and hands focus back here.
        let allow_list_keys = list_owns_keys && !self.files.is_empty() && !search_focused;
        // Left/Right adjust gain here, and the same two keys step the topbar
        // volume fader while it is focused. The fader consumes them first, but
        // `key_presses` deliberately falls back to the raw log for presses
        // another widget consumed, so the gain would move too. This is the only
        // thing that keeps the two apart, and it does not depend on the topbar
        // drawing before the list.
        let allow_list_gain_keys = allow_list_keys && !self.topbar_volume_owns_arrows(ctx);
        if self.debug.cfg.enabled && self.is_list_workspace_active() && !self.files.is_empty() {
            let nav_key_pressed = ctx.input(|i| {
                i.key_pressed(egui::Key::ArrowDown)
                    || i.key_pressed(egui::Key::ArrowUp)
                    || i.key_pressed(egui::Key::PageDown)
                    || i.key_pressed(egui::Key::PageUp)
                    || i.key_pressed(egui::Key::Home)
                    || i.key_pressed(egui::Key::End)
            });
            if nav_key_pressed && !allow_list_keys {
                self.debug_trace_input(&format!(
                    "list nav blocked (search_focused={search_focused}, has_non_list_focus={has_non_list_focus}, list_owns_keys={list_owns_keys})"
                ));
            }
        }
        let list_key_intent = if allow_list_keys {
            ctx.input(|i| {
                i.key_pressed(egui::Key::ArrowDown)
                    || i.key_pressed(egui::Key::ArrowUp)
                    || (!has_non_list_focus && i.key_pressed(egui::Key::Enter))
                    || (allow_list_gain_keys
                        && (i.key_pressed(egui::Key::ArrowLeft)
                            || i.key_pressed(egui::Key::ArrowRight)))
                    || i.key_pressed(egui::Key::PageDown)
                    || i.key_pressed(egui::Key::PageUp)
                    || i.key_pressed(egui::Key::Home)
                    || i.key_pressed(egui::Key::End)
                    || i.key_pressed(egui::Key::Delete)
                    || ((i.modifiers.ctrl || i.modifiers.command) && i.key_pressed(egui::Key::A))
            })
        } else {
            false
        };
        if allow_list_keys && list_key_intent {
            Self::focus_list_widget(ctx);
            list_has_focus = true;
            self.list_has_focus = true;
        }
        if list_has_focus {
            ctx.memory_mut(|m| {
                m.set_focus_lock_filter(
                    list_focus_id,
                    egui::EventFilter {
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        tab: true,
                        ..Default::default()
                    },
                );
            });
        }

        // Counted rather than tested: see `key_presses`. A held arrow must
        // not lose steps to a frame that took longer than the repeat rate.
        let (down_steps, up_steps, pgdown_steps, pgup_steps) = if allow_list_keys {
            (
                key_presses(ctx, egui::Key::ArrowDown),
                key_presses(ctx, egui::Key::ArrowUp),
                key_presses(ctx, egui::Key::PageDown),
                key_presses(ctx, egui::Key::PageUp),
            )
        } else {
            (0, 0, 0, 0)
        };
        let pressed_down = down_steps > 0;
        let pressed_up = up_steps > 0;
        let pressed_pgdown = pgdown_steps > 0;
        let pressed_pgup = pgup_steps > 0;
        let pressed_enter = if allow_list_keys && !has_non_list_focus {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
        } else {
            false
        };
        // Consumed, not just observed: an un-consumed Ctrl+A fires here *and*
        // in whatever text field also saw it. Both spellings are consumed
        // because a raw Ctrl (without egui-winit's paired `command` flag) is
        // what some platforms and the test harness deliver.
        let pressed_ctrl_a = if allow_list_keys {
            ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::A)
                    | i.consume_key(egui::Modifiers::CTRL, egui::Key::A)
            })
        } else {
            false
        };
        let pressed_left = if allow_list_gain_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft))
        } else {
            false
        };
        let pressed_right = if allow_list_gain_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight))
        } else {
            false
        };
        let pressed_home = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Home))
        } else {
            false
        };
        let pressed_end = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::End))
        } else {
            false
        };
        let pressed_delete = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Delete))
        } else {
            false
        };
        let pressed_f2 = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F2))
        } else {
            false
        };

        if self.is_list_workspace_active() && !self.files.is_empty() && allow_list_keys {
            if pressed_ctrl_a
                || pressed_home
                || pressed_end
                || pressed_pgdown
                || pressed_pgup
                || pressed_down
                || pressed_up
                || pressed_enter
                || pressed_delete
                || pressed_left
                || pressed_right
            {
                Self::focus_list_widget(ctx);
                list_has_focus = true;
                self.search_has_focus = false;
            }
            if pressed_ctrl_a {
                self.selected_multi.clear();
                for i in 0..self.files.len() {
                    self.selected_multi.insert(i);
                }
                if self.selected.is_none() {
                    self.selected = Some(0);
                }
            }
            let last = self.files.len().saturating_sub(1);
            // Clamped, not trusted: a filter or a removal can leave `selected`
            // past the end, and stepping from there lands past the end too --
            // where `update_selection_on_click` and `select_and_load` both
            // refuse the row and the arrows stop doing anything at all.
            let cur = self.selected.unwrap_or(0).min(last);
            let target = if pressed_home || pressed_end {
                Some(if pressed_home { 0 } else { last })
            } else if pressed_pgdown || pressed_pgup {
                let page = metrics.visible_rows.max(1) as isize;
                let steps = (pgdown_steps as isize - pgup_steps as isize) * page;
                Some(step_row(cur, steps, last))
            } else if pressed_down || pressed_up {
                let steps = down_steps as isize - up_steps as isize;
                Some(step_row(cur, steps, last))
            } else {
                None
            };
            if let Some(target) = target {
                let mods = ctx.input(|i| i.modifiers);
                self.update_selection_on_click(target, mods);
                self.select_and_load(target, true);
                key_moved = true;
            }
            if pressed_enter && !self.suppress_list_enter {
                let selected = self.selected_paths();
                if !selected.is_empty() {
                    self.open_paths_in_tabs(&selected);
                }
            }
            if pressed_delete {
                let selected = self.selected_paths();
                if !selected.is_empty() {
                    self.remove_paths_from_list_with_undo(&selected);
                }
            }
            if pressed_f2 {
                let renameable = self.selected_renameable_paths();
                if renameable.len() == 1 {
                    self.begin_inline_rename(renameable[0].clone());
                }
            }
            if key_moved && self.auto_play_list_nav {
                self.request_list_autoplay();
            }
            if pressed_left || pressed_right {
                let mods = ctx.input(|i| i.modifiers);
                let step = if mods.shift { 0.1 } else { 1.0 };
                let delta = if pressed_left { -step } else { step };
                let mut indices = self.selected_multi.clone();
                if indices.is_empty() {
                    if let Some(i) = self.selected {
                        indices.insert(i);
                    }
                }
                if !indices.is_empty() {
                    self.adjust_gain_for_indices(&indices, delta);
                }
            }
        }

        // Armed for the check at the top of the next frame: an arrow the list
        // acted on, with no pointer press to explain a focus change, means
        // any focus that has moved by the time we look again was egui's own
        // navigation and belongs back here.
        self.list_arrow_focus_guard = (pressed_down || pressed_up)
            && list_has_focus
            && !ctx.input(|i| i.pointer.any_pressed());

        ListInteractionState {
            key_moved,
            list_has_focus,
        }
    }
}
