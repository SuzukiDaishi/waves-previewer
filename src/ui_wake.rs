//! A process-wide handle for waking the UI thread from a background thread.
//!
//! The frame loop only asks egui for another frame when it has a reason to
//! (see `run_frame_finish`). That lets an idle app sleep instead of
//! repainting forever, but it also means a thread that pushes work into a
//! channel the UI polls -- the IPC listener, the folder watcher -- would
//! have its message sit unread until the user happened to move the mouse.
//!
//! Those threads call [`wake_ui`] after sending. The app registers its
//! `egui::Context` on the first frame; before that (and in headless tests)
//! `wake_ui` is a no-op, which is correct: there is no window to wake.

use std::sync::OnceLock;

static UI_CONTEXT: OnceLock<egui::Context> = OnceLock::new();

/// Publish the context background threads should wake. Called once, from
/// the first frame; later calls are ignored.
pub fn register_ui_context(ctx: &egui::Context) {
    let _ = UI_CONTEXT.set(ctx.clone());
}

/// Ask for one more frame. Safe to call from any thread, and cheap enough
/// to call per message.
pub fn wake_ui() {
    if let Some(ctx) = UI_CONTEXT.get() {
        ctx.request_repaint();
    }
}

/// True once a context has been registered. Only used by tests and by the
/// idle check, which must not assume a window exists.
pub fn is_registered() -> bool {
    UI_CONTEXT.get().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waking_without_a_registered_context_is_a_no_op() {
        // Headless tests never register one; this must not panic.
        if !is_registered() {
            wake_ui();
        }
    }
}
