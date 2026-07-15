use std::time::{Duration, Instant};

use super::types::{Toast, ToastSeverity};
use super::WavesPreviewer;

const TOAST_MAX_STACK: usize = 5;
const TOAST_LIFETIME: Duration = Duration::from_secs(6);
const TOAST_LIFETIME_ERROR: Duration = Duration::from_secs(10);

fn toast_lifetime(severity: ToastSeverity) -> Duration {
    match severity {
        ToastSeverity::Error => TOAST_LIFETIME_ERROR,
        _ => TOAST_LIFETIME,
    }
}

impl WavesPreviewer {
    /// Queue a user-visible notification. Also mirrors the message into the
    /// debug log so the F12 window keeps a single funnel of events.
    pub(crate) fn push_toast(&mut self, severity: ToastSeverity, msg: impl Into<String>) {
        let message = msg.into();
        self.debug_log(format!("toast [{severity:?}]: {message}"));
        if let Some(last) = self.toasts.last_mut() {
            if last.message == message && last.severity == severity {
                last.count = last.count.saturating_add(1);
                last.created_at = Instant::now();
                return;
            }
        }
        if self.toasts.len() >= TOAST_MAX_STACK {
            self.toasts.remove(0);
        }
        self.toasts.push(Toast {
            message,
            severity,
            created_at: Instant::now(),
            count: 1,
        });
    }

    /// Editing buffers keep float headroom instead of hard-clipping, so warn
    /// once when an edit leaves peaks above full scale: they will clip at
    /// export/playback boundaries unless gain is reduced.
    pub(super) fn notify_if_tab_over_fs(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let peak = tab
            .ch_samples
            .iter()
            .flat_map(|ch| ch.iter())
            .fold(0.0f32, |m, v| m.max(v.abs()));
        if peak > 1.0 {
            let db = 20.0 * peak.log10();
            self.push_toast(
                ToastSeverity::Info,
                format!("Peak +{db:.1} dB above 0 dBFS — audio will clip on export unless gain is reduced"),
            );
        }
    }

    pub(super) fn ui_toast_overlay(&mut self, ctx: &egui::Context) {
        self.toasts
            .retain(|t| t.created_at.elapsed() < toast_lifetime(t.severity));
        if self.toasts.is_empty() {
            return;
        }
        // Keep repainting so toasts expire even without input events.
        ctx.request_repaint_after(Duration::from_millis(250));
        // Anchored below the topbar so topbar pixel checks never see toast
        // pixels; clicking a toast dismisses it.
        let mut dismiss: Option<usize> = None;
        egui::Area::new(egui::Id::new("toast_overlay"))
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 72.0))
            .order(egui::Order::Foreground)
            .interactable(true)
            .show(ctx, |ui| {
                for (idx, toast) in self.toasts.iter().enumerate() {
                    let (accent, title) = match toast.severity {
                        ToastSeverity::Info => (ui.style().visuals.text_color(), "Info"),
                        ToastSeverity::Warning => (ui.style().visuals.warn_fg_color, "Warning"),
                        ToastSeverity::Error => (ui.style().visuals.error_fg_color, "Error"),
                    };
                    let frame = egui::Frame::window(ui.style())
                        .stroke(egui::Stroke::new(1.5, accent))
                        .inner_margin(egui::Margin::symmetric(10, 8));
                    let resp = frame
                        .show(ui, |ui| {
                            ui.set_max_width(360.0);
                            let mut text = toast.message.clone();
                            if toast.count > 1 {
                                text.push_str(&format!(" (x{})", toast.count));
                            }
                            ui.label(
                                egui::RichText::new(title)
                                    .color(accent)
                                    .small()
                                    .strong(),
                            );
                            ui.label(egui::RichText::new(text).small());
                        })
                        .response;
                    if resp.interact(egui::Sense::click()).clicked() {
                        dismiss = Some(idx);
                    }
                    ui.add_space(6.0);
                }
            });
        if let Some(idx) = dismiss {
            if idx < self.toasts.len() {
                self.toasts.remove(idx);
            }
        }
    }
}
