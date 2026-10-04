//! The mini meter's spectrum as one filled shape.
//!
//! It used to be one `rect_filled` per display column. At a display scale
//! that is not a whole number (125 %, 150 %) a column edge falls inside a
//! physical pixel, and two anti-aliased edges meeting there each cover it
//! only partly: the panel shows through as a dark vertical line. A mesh
//! whose neighbouring columns share their vertices has no edge between
//! them to feather, so there is nothing to show through.

use egui::{Color32, Mesh, Pos2, Rect};

/// The spectrum `db` (one value per column, left to right) as a band from
/// the bottom of `rect` up to each column's level, `floor` dB at the
/// bottom and 0 dB `headroom` points below the top. Each column's colour
/// comes from `color(t, norm)`, `t` its place across (0..1) and `norm` its
/// height (0..1).
pub fn spectrum_mesh(
    rect: Rect,
    db: &[f32],
    floor: f32,
    headroom: f32,
    color: impl Fn(f32, f32) -> Color32,
) -> Mesh {
    let mut mesh = Mesh::default();
    let cols = db.len();
    if cols == 0 || rect.width() <= 0.0 {
        return mesh;
    }
    let height = (rect.height() - headroom).max(0.0);
    let norm = |value: f32| ((value - floor) / -floor).clamp(0.0, 1.0);
    // One edge per column boundary, shared by the columns on both sides of
    // it: its height and colour are those of the column it starts, the last
    // one closing the final column.
    for edge in 0..=cols {
        let t = edge as f32 / cols as f32;
        let x = rect.left() + t * rect.width();
        let n = norm(db[edge.min(cols - 1)]);
        let c = color(t, n);
        mesh.colored_vertex(Pos2::new(x, rect.bottom() - n * height), c);
        mesh.colored_vertex(Pos2::new(x, rect.bottom()), c);
    }
    for col in 0..cols as u32 {
        let (top_l, bottom_l, top_r, bottom_r) = (2 * col, 2 * col + 1, 2 * col + 2, 2 * col + 3);
        mesh.add_triangle(top_l, bottom_l, top_r);
        mesh.add_triangle(top_r, bottom_l, bottom_r);
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neighbouring_columns_share_their_edge() {
        let rect = Rect::from_min_max(Pos2::new(10.0, 0.0), Pos2::new(110.0, 52.0));
        let db = [-84.0, -42.0, 0.0, -21.0];
        let mesh = spectrum_mesh(rect, &db, -84.0, 12.0, |_, _| Color32::WHITE);
        // One pair of vertices per boundary, not two pairs per column.
        assert_eq!(mesh.vertices.len(), 2 * (db.len() + 1));
        assert_eq!(mesh.indices.len(), 6 * db.len());
        // Boundaries are evenly spaced from edge to edge, with no gap.
        let xs: Vec<f32> = mesh.vertices.iter().step_by(2).map(|v| v.pos.x).collect();
        assert_eq!(xs, vec![10.0, 35.0, 60.0, 85.0, 110.0]);
        // The bottom is flat; a column's top is its level.
        assert!(mesh
            .vertices
            .iter()
            .skip(1)
            .step_by(2)
            .all(|v| v.pos.y == 52.0));
        let tops: Vec<f32> = mesh.vertices.iter().step_by(2).map(|v| v.pos.y).collect();
        assert_eq!(tops, vec![52.0, 32.0, 12.0, 22.0, 22.0]);
        // Every triangle uses vertices of one column and its right neighbour.
        assert!(mesh
            .indices
            .chunks(3)
            .all(|tri| tri.iter().max().unwrap() - tri.iter().min().unwrap() <= 3));
    }

    #[test]
    fn nothing_to_draw_is_an_empty_mesh() {
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(100.0, 50.0));
        assert!(spectrum_mesh(rect, &[], -84.0, 12.0, |_, _| Color32::WHITE).is_empty());
    }
}
