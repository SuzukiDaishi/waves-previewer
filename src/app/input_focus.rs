use std::collections::HashSet;

use egui::{Id, LayerId, Rect, Response};

use super::{WavesPreviewer, WorkspaceView};

/// A UI surface that can own pointer and keyboard input.
///
/// This is intentionally runtime-only. It describes UI ownership, not document
/// state, so it must never be serialized into preferences or sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum UiSurface {
    List,
    Editor,
    EffectGraph,
    Recording,
    Floating(Id),
}

#[derive(Clone, Copy, Debug)]
struct UiSurfaceRegion {
    target: UiSurface,
    layer_id: LayerId,
    rect: Rect,
}

/// The input context for one frame: who is allowed to act on a key press.
///
/// Resolved once per frame, before any handler runs, so every handler answers
/// the question the same way. `text_editing` is the innermost scope -- while a
/// caret is live nothing else may claim a key that could become a character.
///
/// Note `text_editing` is true for a focused `DragValue` too: egui renders one
/// as a `TextEdit` sharing the same `Id` the moment it takes focus, so there is
/// no focused-but-not-editing state to distinguish. Guards that must survive a
/// focused DragValue (the list's arrow keys) use widget focus instead.
#[derive(Clone, Copy, Debug)]
pub(super) struct InputScope {
    pub(super) text_editing: bool,
    pub(super) owner: UiSurface,
}

impl Default for InputScope {
    fn default() -> Self {
        Self {
            text_editing: false,
            owner: UiSurface::List,
        }
    }
}

/// Routes pointer and keyboard input to one UI surface.
///
/// Wheel input follows the pointer: the top-most registered surface under the
/// cursor gets it, the way a browser routes scrolling. Keyboard commands follow
/// the last surface the user entered (click, or a newly opened window), so a
/// dialog owns Delete/Ctrl+A/Space while it is up without having to block the
/// background.
///
/// Hit regions are retained for one frame so focus can be selected before the
/// next frame starts drawing. This matters for the virtualized List, which
/// reads wheel input while it is being built rather than from an egui
/// `ScrollArea` response, and for keyboard handlers that run during the
/// CentralPanel -- before this frame's dialogs have registered themselves.
#[derive(Debug, Default)]
pub(super) struct UiInputFocusState {
    active: Option<UiSurface>,
    pointer: Option<UiSurface>,
    history: Vec<UiSurface>,
    previous_regions: Vec<UiSurfaceRegion>,
    current_regions: Vec<UiSurfaceRegion>,
    previous_visible: HashSet<UiSurface>,
    current_visible: HashSet<UiSurface>,
}

/// Temporarily hides wheel/trackpad deltas while a surface the pointer is not
/// over is being built, then restores them for the surface that owns them.
pub(super) struct PointerScrollInputGuard {
    ctx: egui::Context,
    saved_smooth_delta: Option<egui::Vec2>,
}

impl PointerScrollInputGuard {
    fn new(ctx: &egui::Context, allow_scroll: bool) -> Self {
        let saved_smooth_delta = (!allow_scroll).then(|| {
            ctx.input_mut(|input| {
                let saved = input.smooth_scroll_delta;
                input.smooth_scroll_delta = egui::Vec2::ZERO;
                saved
            })
        });
        Self {
            ctx: ctx.clone(),
            saved_smooth_delta,
        }
    }
}

impl Drop for PointerScrollInputGuard {
    fn drop(&mut self) {
        if let Some(saved) = self.saved_smooth_delta.take() {
            self.ctx
                .input_mut(|input| input.smooth_scroll_delta = saved);
        }
    }
}

impl UiInputFocusState {
    pub(super) fn begin_frame(&mut self, ctx: &egui::Context, fallback: UiSurface) {
        self.current_regions.clear();
        self.current_visible.clear();

        if self.active.is_none() {
            self.activate(fallback);
        }

        self.pointer = self.resolve_pointer_surface(ctx);

        let click_pos = ctx.input(|input| {
            input
                .pointer
                .any_pressed()
                .then(|| input.pointer.interact_pos())
                .flatten()
        });
        let Some(pos) = click_pos else {
            return;
        };
        if let Some(target) = self.region_at(ctx, pos) {
            self.activate(target);
        }
    }

    /// The surface the wheel belongs to this frame.
    ///
    /// Unregistered overlays (toasts, the mascot) are interactable Areas that
    /// sit on top of everything, so a pointer over one resolves to no region at
    /// all. Falling back to the active surface there keeps scrolling alive
    /// while a toast drifts under the cursor.
    fn resolve_pointer_surface(&self, ctx: &egui::Context) -> Option<UiSurface> {
        let pos = ctx.input(|input| input.pointer.hover_pos())?;
        self.region_at(ctx, pos).or(self.active)
    }

    /// The registered surface owning `pos`, honouring layer order.
    ///
    /// Reverse registration order is a deterministic tie-breaker for
    /// non-overlapping workspace regions that share the background layer.
    fn region_at(&self, ctx: &egui::Context, pos: egui::Pos2) -> Option<UiSurface> {
        let top_layer = ctx.layer_id_at(pos)?;
        self.previous_regions
            .iter()
            .rev()
            .find(|region| region.layer_id == top_layer && region.rect.contains(pos))
            .map(|region| region.target)
    }

    /// Marks a surface as visible before its contents are drawn. A newly
    /// opened surface receives focus immediately, so its first rendered
    /// `ScrollArea` is already configured correctly.
    pub(super) fn begin_surface(&mut self, target: UiSurface) {
        let newly_visible =
            !self.previous_visible.contains(&target) && !self.current_visible.contains(&target);
        self.current_visible.insert(target);
        if newly_visible {
            self.activate(target);
        }
    }

    pub(super) fn register_response(&mut self, target: UiSurface, response: &Response) {
        self.register_region(target, response.layer_id, response.rect);
    }

    pub(super) fn register_region(&mut self, target: UiSurface, layer_id: LayerId, rect: Rect) {
        self.current_visible.insert(target);
        self.current_regions.push(UiSurfaceRegion {
            target,
            layer_id,
            rect,
        });
    }

    pub(super) fn is_active(&self, target: UiSurface) -> bool {
        self.active == Some(target)
    }

    fn owns_pointer(&self, target: UiSurface) -> bool {
        self.pointer == Some(target)
    }

    pub(super) fn allows_pointer_scroll(
        &self,
        target: UiSurface,
        ctx: &egui::Context,
        layer_id: LayerId,
        rect: Rect,
    ) -> bool {
        if !self.owns_pointer(target) {
            return false;
        }
        let Some(pos) = ctx.input(|input| input.pointer.hover_pos()) else {
            return false;
        };
        rect.contains(pos) && ctx.layer_id_at(pos) == Some(layer_id)
    }

    pub(super) fn scroll_source(&self, target: UiSurface) -> egui::scroll_area::ScrollSource {
        if self.owns_pointer(target) {
            egui::scroll_area::ScrollSource::ALL
        } else {
            egui::scroll_area::ScrollSource::SCROLL_BAR | egui::scroll_area::ScrollSource::DRAG
        }
    }

    pub(super) fn finish_frame(&mut self, fallback: UiSurface) {
        self.history
            .retain(|target| self.current_visible.contains(target));

        if self
            .active
            .is_none_or(|target| !self.current_visible.contains(&target))
        {
            let next = self
                .history
                .last()
                .copied()
                .filter(|target| self.current_visible.contains(target))
                .or_else(|| self.current_visible.contains(&fallback).then_some(fallback));
            self.active = None;
            if let Some(next) = next {
                self.activate(next);
            }
        }

        std::mem::swap(&mut self.previous_regions, &mut self.current_regions);
        self.current_regions.clear();
        std::mem::swap(&mut self.previous_visible, &mut self.current_visible);
        self.current_visible.clear();
    }

    fn activate(&mut self, target: UiSurface) {
        self.history.retain(|candidate| *candidate != target);
        self.history.push(target);
        self.active = Some(target);
    }

    pub(super) fn active(&self) -> Option<UiSurface> {
        self.active
    }
}

impl WavesPreviewer {
    pub(super) fn current_ui_surface(&self) -> UiSurface {
        match self.workspace_view {
            WorkspaceView::EffectGraph => UiSurface::EffectGraph,
            WorkspaceView::Recording => UiSurface::Recording,
            WorkspaceView::Editor if self.active_tab.is_some() => UiSurface::Editor,
            WorkspaceView::Editor | WorkspaceView::List => UiSurface::List,
        }
    }

    /// Resolve this frame's input scope. Called once, before any handler runs.
    pub(super) fn resolve_input_scope(&mut self, ctx: &egui::Context) {
        self.input_scope = InputScope {
            text_editing: ctx.text_edit_focused(),
            owner: self
                .ui_input_focus
                .active()
                .unwrap_or_else(|| self.current_ui_surface()),
        };
    }

    /// Whether an application-wide chord may fire.
    ///
    /// These are the modified chords that cannot be typed as text -- Ctrl+S,
    /// Ctrl+Z, Ctrl+1.., F9/F12. They work from anywhere except while a caret
    /// is live (`consume_key` does not suppress the matching `Event::Text`, so
    /// the caret has to be checked, not merely out-consumed) or while the
    /// keymap window is waiting to capture a chord.
    pub(super) fn global_keys_allowed(&self) -> bool {
        !self.input_scope.text_editing && self.keymap_capture.is_none()
    }

    /// Whether a key that belongs to the workspace as a whole may fire.
    ///
    /// Transport and volume are not tied to one pane, but they must still
    /// stand down while a floating window is the surface the user is in --
    /// otherwise Space toggles playback while a dialog is up.
    pub(super) fn workspace_keys_allowed(&self) -> bool {
        self.global_keys_allowed() && !matches!(self.input_scope.owner, UiSurface::Floating(_))
    }

    /// Whether a surface-scoped key may fire for `target`.
    ///
    /// These are the unmodified keys and per-surface commands -- Space, Delete,
    /// arrows, digits, Ctrl+A, Ctrl+C/V, F2. They only fire while the user is
    /// inside that surface, so an open dialog silently owns them.
    pub(super) fn surface_keys_allowed(&self, target: UiSurface) -> bool {
        self.global_keys_allowed() && self.input_scope.owner == target
    }

    pub(super) fn begin_floating_scroll_surface(&mut self, id: impl std::hash::Hash) -> UiSurface {
        let target = UiSurface::Floating(Id::new(id));
        self.ui_input_focus.begin_surface(target);
        target
    }

    pub(super) fn register_scroll_surface(&mut self, target: UiSurface, response: &Response) {
        self.ui_input_focus.register_response(target, response);
    }

    pub(super) fn scroll_source_for(&self, target: UiSurface) -> egui::scroll_area::ScrollSource {
        self.ui_input_focus.scroll_source(target)
    }

    pub(super) fn pointer_scroll_input_guard(
        &self,
        target: UiSurface,
        ctx: &egui::Context,
    ) -> PointerScrollInputGuard {
        PointerScrollInputGuard::new(ctx, self.ui_input_focus.owns_pointer(target))
    }

    pub(super) fn allows_pointer_scroll(
        &self,
        target: UiSurface,
        ui: &egui::Ui,
        rect: Rect,
    ) -> bool {
        self.ui_input_focus
            .allows_pointer_scroll(target, ui.ctx(), ui.layer_id(), rect)
    }
}
