//! The list's Comments column.
//!
//! The shared session's conversation, seen one row at a time: how much has
//! been said about this file, whether any of it is still open, whether any of
//! it is new to this reader -- and, on a click, the threads themselves with a
//! composer already pointed at the row.
//!
//! The window is still where a conversation is read at length. This is where
//! it is *noticed*, next to the file it is about, while going down a list.

use std::path::Path;

use egui::{Color32, Sense};

/// Marks a comment that is new to this reader; the same blue the window's
/// unread dot uses.
const UNREAD: Color32 = Color32::from_rgb(140, 190, 240);
/// A thread nobody has settled yet.
const OPEN: Color32 = Color32::from_rgb(240, 190, 90);

impl crate::app::WavesPreviewer {
    /// Draw one row's comment badge and, while it is open, its popup.
    ///
    /// Returns whether the cell took the click, so the row does not also read
    /// it as a click on the row.
    pub(super) fn ui_list_comment_cell(
        &mut self,
        ui: &mut egui::Ui,
        path: &Path,
        row_h: f32,
        text_height: f32,
    ) -> bool {
        // One hash probe per row: the counting happens once per change to the
        // conversation, in `refresh_comment_path_index`.
        let summary = self.comment_summary_for_path(path);
        // Bound to the file, not to the table's auto-id sequence. The popup
        // is addressed by this id, and a sort or a scroll that moved the row
        // would otherwise leave it open over whichever file inherited the
        // number -- the same reason the Gain cell pins its own id.
        let id_source = ("list_comments", path);
        let (rect, response) = ui
            .push_id(id_source, |ui| {
                ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), row_h * 0.9),
                    Sense::click(),
                )
            })
            .inner;
        let font = egui::FontId::proportional((text_height * 0.9).max(9.0));
        let origin = rect.left_center() + egui::vec2(6.0, 0.0);
        if summary.is_empty() {
            // Nothing to report, so nothing is drawn until the pointer is
            // here: a list of a hundred thousand files should not carry a
            // hundred thousand grey speech bubbles to say so.
            if response.hovered() {
                ui.painter().text(
                    origin,
                    egui::Align2::LEFT_CENTER,
                    "\u{1F4AC}+",
                    font,
                    ui.visuals().weak_text_color(),
                );
            }
        } else {
            let color = if summary.open > 0 {
                ui.visuals().text_color()
            } else {
                // Every thread settled: still there, no longer asking.
                ui.visuals().weak_text_color()
            };
            let galley = ui.painter().layout_no_wrap(
                format!("\u{1F4AC} {}", summary.total),
                font.clone(),
                color,
            );
            let text_end = origin.x + galley.size().x;
            ui.painter().galley(
                egui::pos2(origin.x, origin.y - galley.size().y * 0.5),
                galley,
                color,
            );
            if summary.unread > 0 {
                ui.painter().text(
                    egui::pos2(text_end + 4.0, origin.y),
                    egui::Align2::LEFT_CENTER,
                    "\u{25CF}",
                    egui::FontId::proportional((text_height * 0.75).max(8.0)),
                    UNREAD,
                );
            } else if summary.open > 0 && summary.total > summary.open {
                ui.painter().text(
                    egui::pos2(text_end + 4.0, origin.y),
                    egui::Align2::LEFT_CENTER,
                    "\u{25CB}",
                    egui::FontId::proportional((text_height * 0.75).max(8.0)),
                    OPEN,
                );
            }
        }
        if response.hovered() {
            ui.painter().rect_stroke(
                rect,
                4.0,
                egui::Stroke::new(1.0, ui.visuals().widgets.hovered.bg_stroke.color),
                egui::StrokeKind::Inside,
            );
        }
        // The badge is painted, not built out of widgets, so what it says has
        // to be stated for the accessibility tree -- which is also what makes
        // the cell addressable from a test.
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, comment_cell_label(summary))
        });
        let response = response.on_hover_text(comment_cell_hover(summary));
        let took_click = response.clicked();
        egui::Popup::from_toggle_button_response(&response)
            // A composer lives in here: closing on the click that puts the
            // caret in it would make the column unusable for writing, which
            // is half of what it is for.
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                self.ui_comment_row_popup(ui, path);
            });
        took_click
    }
}

/// What the painted badge says, for the accessibility tree.
fn comment_cell_label(summary: crate::app::comment_ops::CommentPathSummary) -> String {
    if summary.is_empty() {
        return "Comments: none".to_string();
    }
    let mut label = format!("Comments: {}", summary.total);
    if summary.unread > 0 {
        label.push_str(&format!(", {} unread", summary.unread));
    }
    if summary.open == 0 {
        label.push_str(", all resolved");
    }
    label
}

fn comment_cell_hover(summary: crate::app::comment_ops::CommentPathSummary) -> String {
    if summary.is_empty() {
        return "No comments about this file yet.\nClick to write one for the team.".to_string();
    }
    let mut text = format!(
        "{} comment{} about this file",
        summary.total,
        if summary.total == 1 { "" } else { "s" }
    );
    if summary.unread > 0 {
        text.push_str(&format!(" · {} unread", summary.unread));
    }
    text.push_str(if summary.open > 0 {
        "\nClick to read, reply, or resolve."
    } else {
        " · every thread resolved\nClick to read or reopen."
    });
    text
}

#[cfg(test)]
mod tests {
    use super::{comment_cell_hover, comment_cell_label};
    use crate::app::comment_ops::CommentPathSummary;

    #[test]
    fn the_painted_badge_states_itself_for_a_reader_who_cannot_see_it() {
        assert_eq!(
            comment_cell_label(CommentPathSummary::default()),
            "Comments: none"
        );
        assert_eq!(
            comment_cell_label(CommentPathSummary {
                total: 3,
                open: 1,
                unread: 2,
            }),
            "Comments: 3, 2 unread"
        );
        assert_eq!(
            comment_cell_label(CommentPathSummary {
                total: 2,
                open: 0,
                unread: 0,
            }),
            "Comments: 2, all resolved"
        );
    }

    #[test]
    fn an_empty_cell_says_what_a_click_would_do() {
        let hover = comment_cell_hover(CommentPathSummary::default());
        assert!(hover.contains("No comments"));
        assert!(hover.contains("Click"));
    }

    #[test]
    fn one_comment_is_not_reported_as_one_comments() {
        let hover = comment_cell_hover(CommentPathSummary {
            total: 1,
            open: 1,
            unread: 0,
        });
        assert!(hover.starts_with("1 comment about"), "{hover}");
    }

    #[test]
    fn unread_and_settled_are_both_said_out_loud() {
        let unread = comment_cell_hover(CommentPathSummary {
            total: 3,
            open: 3,
            unread: 2,
        });
        assert!(unread.contains("3 comments"), "{unread}");
        assert!(unread.contains("2 unread"), "{unread}");

        let settled = comment_cell_hover(CommentPathSummary {
            total: 2,
            open: 0,
            unread: 0,
        });
        assert!(settled.contains("every thread resolved"), "{settled}");
    }
}
