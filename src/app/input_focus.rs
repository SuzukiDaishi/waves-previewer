use std::collections::HashSet;

use egui::{Id, LayerId, Rect, Response};

use super::{WavesPreviewer, WorkspaceView};

/// A click-selected surface that may receive wheel/trackpad scrolling.
///
/// This is intentionally runtime-only. It describes UI ownership, not document
/// state, so it must never be serialized into preferences or sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum UiScrollTarget {
    List,
    Editor,
    EffectGraph,
    Recording,
    Floating(Id),
}

#[derive(Clone, Copy, Debug)]
struct UiScrollRegion {
    target: UiScrollTarget,
    layer_id: LayerId,
    rect: Rect,
}

/// Routes scroll-like pointer input to one click-selected UI surface.
///
/// Hit regions are retained for one frame so focus can be selected before the
/// next frame starts drawing. This matters for the virtualized List, which
/// reads wheel input while it is being built rather than from an egui
/// `ScrollArea` response.
#[derive(Debug, Default)]
pub(super) struct UiScrollFocusState {
    active: Option<UiScrollTarget>,
    history: Vec<UiScrollTarget>,
    previous_regions: Vec<UiScrollRegion>,
    current_regions: Vec<UiScrollRegion>,
    previous_visible: HashSet<UiScrollTarget>,
    current_visible: HashSet<UiScrollTarget>,
}

/// Temporarily hides wheel/trackpad deltas while an inactive surface is being
/// built, then restores them for the active surface rendered later.
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

impl UiScrollFocusState {
    pub(super) fn begin_frame(&mut self, ctx: &egui::Context, fallback: UiScrollTarget) {
        self.current_regions.clear();
        self.current_visible.clear();

        if self.active.is_none() {
            self.activate(fallback);
        }

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
        let Some(top_layer) = ctx.layer_id_at(pos) else {
            return;
        };

        // Reverse registration order is a deterministic tie-breaker for
        // non-overlapping workspace regions that share the background layer.
        if let Some(target) = self
            .previous_regions
            .iter()
            .rev()
            .find(|region| region.layer_id == top_layer && region.rect.contains(pos))
            .map(|region| region.target)
        {
            self.activate(target);
        }
    }

    /// Marks a surface as visible before its contents are drawn. A newly
    /// opened surface receives focus immediately, so its first rendered
    /// `ScrollArea` is already configured correctly.
    pub(super) fn begin_surface(&mut self, target: UiScrollTarget) {
        let newly_visible =
            !self.previous_visible.contains(&target) && !self.current_visible.contains(&target);
        self.current_visible.insert(target);
        if newly_visible {
            self.activate(target);
        }
    }

    pub(super) fn register_response(&mut self, target: UiScrollTarget, response: &Response) {
        self.register_region(target, response.layer_id, response.rect);
    }

    pub(super) fn register_region(
        &mut self,
        target: UiScrollTarget,
        layer_id: LayerId,
        rect: Rect,
    ) {
        self.current_visible.insert(target);
        self.current_regions.push(UiScrollRegion {
            target,
            layer_id,
            rect,
        });
    }

    pub(super) fn is_active(&self, target: UiScrollTarget) -> bool {
        self.active == Some(target)
    }

    pub(super) fn allows_pointer_scroll(
        &self,
        target: UiScrollTarget,
        ctx: &egui::Context,
        layer_id: LayerId,
        rect: Rect,
    ) -> bool {
        if !self.is_active(target) {
            return false;
        }
        let Some(pos) = ctx.input(|input| input.pointer.hover_pos()) else {
            return false;
        };
        rect.contains(pos) && ctx.layer_id_at(pos) == Some(layer_id)
    }

    pub(super) fn scroll_source(&self, target: UiScrollTarget) -> egui::scroll_area::ScrollSource {
        if self.is_active(target) {
            egui::scroll_area::ScrollSource::ALL
        } else {
            egui::scroll_area::ScrollSource::SCROLL_BAR | egui::scroll_area::ScrollSource::DRAG
        }
    }

    pub(super) fn finish_frame(&mut self, fallback: UiScrollTarget) {
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

    fn activate(&mut self, target: UiScrollTarget) {
        self.history.retain(|candidate| *candidate != target);
        self.history.push(target);
        self.active = Some(target);
    }

    #[cfg(feature = "kittest")]
    pub(super) fn active(&self) -> Option<UiScrollTarget> {
        self.active
    }
}

impl WavesPreviewer {
    pub(super) fn current_ui_scroll_target(&self) -> UiScrollTarget {
        match self.workspace_view {
            WorkspaceView::EffectGraph => UiScrollTarget::EffectGraph,
            WorkspaceView::Recording => UiScrollTarget::Recording,
            WorkspaceView::Editor if self.active_tab.is_some() => UiScrollTarget::Editor,
            WorkspaceView::Editor | WorkspaceView::List => UiScrollTarget::List,
        }
    }

    pub(super) fn begin_floating_scroll_surface(
        &mut self,
        id: impl std::hash::Hash,
    ) -> UiScrollTarget {
        let target = UiScrollTarget::Floating(Id::new(id));
        self.ui_scroll_focus.begin_surface(target);
        target
    }

    pub(super) fn register_scroll_surface(&mut self, target: UiScrollTarget, response: &Response) {
        self.ui_scroll_focus.register_response(target, response);
    }

    pub(super) fn scroll_source_for(
        &self,
        target: UiScrollTarget,
    ) -> egui::scroll_area::ScrollSource {
        self.ui_scroll_focus.scroll_source(target)
    }

    pub(super) fn pointer_scroll_input_guard(
        &self,
        target: UiScrollTarget,
        ctx: &egui::Context,
    ) -> PointerScrollInputGuard {
        PointerScrollInputGuard::new(ctx, self.ui_scroll_focus.is_active(target))
    }

    pub(super) fn allows_pointer_scroll(
        &self,
        target: UiScrollTarget,
        ui: &egui::Ui,
        rect: Rect,
    ) -> bool {
        self.ui_scroll_focus
            .allows_pointer_scroll(target, ui.ctx(), ui.layer_id(), rect)
    }
}
