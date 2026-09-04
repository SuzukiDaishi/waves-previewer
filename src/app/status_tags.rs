//! Per-row workflow labels: a single `Status` and any number of `Tags`.
//!
//! Both are the same shape — an ordered palette of `LabelDef`s that rows point
//! at by a stable id — so one type serves both and the list, the manager
//! window, prefs and the session all speak it.
//!
//! The id is a slug derived from the label at creation time, never a counter.
//! A `.nwsess` on a file server may have two writers (AGENTS.md), and two
//! people adding a label at the same time would both claim `status_4`; a slug
//! collides only when the labels themselves collide, and then it is
//! disambiguated from the label rather than from a shared sequence. It also
//! keeps a hand-read session file legible.
//!
//! Rows hold `Arc<str>` clones of the palette's ids, so a million rows sharing
//! one status share one allocation (the same interning `display_folder` uses).

use std::sync::Arc;

/// One entry of a palette. `id` is stable for the life of the entry; `label`
/// and `color` are free to change without touching a single row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabelDef {
    pub id: Arc<str>,
    pub label: String,
    pub color: [u8; 3],
}

impl LabelDef {
    pub fn color32(&self) -> egui::Color32 {
        egui::Color32::from_rgb(self.color[0], self.color[1], self.color[2])
    }

    /// Text color that stays readable on `color32()`.
    pub fn text_color(&self) -> egui::Color32 {
        text_color_on(self.color)
    }
}

/// Readable ink for a filled badge, by perceived luminance of the fill.
pub fn text_color_on(rgb: [u8; 3]) -> egui::Color32 {
    let [r, g, b] = rgb.map(f32::from);
    let luma = 0.299 * r + 0.587 * g + 0.114 * b;
    if luma > 150.0 {
        egui::Color32::from_rgb(20, 22, 26)
    } else {
        egui::Color32::WHITE
    }
}

/// An ordered set of labels. Vec order *is* display order, so reordering in
/// the manager window is a `Vec` move and costs nothing per row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LabelPalette {
    pub defs: Vec<LabelDef>,
}

impl LabelPalette {
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn get(&self, id: &str) -> Option<&LabelDef> {
        self.defs.iter().find(|def| &*def.id == id)
    }

    pub fn position(&self, id: &str) -> Option<usize> {
        self.defs.iter().position(|def| &*def.id == id)
    }

    /// The palette's own `Arc` for `id`, so an assignment shares the
    /// allocation instead of making a per-row copy of the string.
    pub fn interned(&self, id: &str) -> Option<Arc<str>> {
        self.get(id).map(|def| Arc::clone(&def.id))
    }

    /// Display label for an id. An id with no definition — a session authored
    /// against a palette this app has never seen — keeps its raw id rather
    /// than disappearing, because dropping it would silently discard the
    /// author's data on the next save.
    pub fn label_for(&self, id: &str) -> String {
        self.get(id)
            .map(|def| def.label.clone())
            .unwrap_or_else(|| id.to_string())
    }

    /// Fill color for an id; unknown ids get a neutral grey so they read as
    /// "defined elsewhere" rather than as one of this palette's entries.
    pub fn color_for(&self, id: &str) -> [u8; 3] {
        self.get(id).map(|def| def.color).unwrap_or(UNKNOWN_COLOR)
    }

    /// Add a label, deriving a unique id from `label`. Returns the new id.
    pub fn add(&mut self, label: &str, color: [u8; 3]) -> Arc<str> {
        let id: Arc<str> = Arc::from(self.unique_id(&slugify(label)).as_str());
        let label = if label.trim().is_empty() {
            id.to_string()
        } else {
            label.trim().to_string()
        };
        self.defs.push(LabelDef {
            id: Arc::clone(&id),
            label,
            color,
        });
        id
    }

    /// Insert a definition read from prefs or a session, keeping its stored
    /// id. A duplicate id is skipped rather than renamed: the id is what rows
    /// point at, so inventing a new one here would orphan them.
    pub fn insert_stored(&mut self, id: &str, label: &str, color: [u8; 3]) -> bool {
        let id = id.trim();
        if id.is_empty() || self.get(id).is_some() {
            return false;
        }
        self.defs.push(LabelDef {
            id: Arc::from(id),
            label: if label.trim().is_empty() {
                id.to_string()
            } else {
                label.trim().to_string()
            },
            color,
        });
        true
    }

    /// Make sure `id` resolves, inventing a placeholder definition when it
    /// does not. Used when a session assigns an id its own palette omits, so
    /// the label survives a round trip instead of being silently dropped.
    pub fn ensure_placeholder(&mut self, id: &str) {
        self.insert_stored(id, id, UNKNOWN_COLOR);
    }

    pub fn rename(&mut self, id: &str, label: &str) {
        if let Some(def) = self.defs.iter_mut().find(|def| &*def.id == id) {
            def.label = label.to_string();
        }
    }

    pub fn set_color(&mut self, id: &str, color: [u8; 3]) {
        if let Some(def) = self.defs.iter_mut().find(|def| &*def.id == id) {
            def.color = color;
        }
    }

    /// Move the entry at `from` to `to`, shifting the rest. Returns whether
    /// anything moved.
    pub fn move_def(&mut self, from: usize, to: usize) -> bool {
        if from >= self.defs.len() || to >= self.defs.len() || from == to {
            return false;
        }
        let def = self.defs.remove(from);
        self.defs.insert(to, def);
        true
    }

    /// Remove one definition. Rows still pointing at it are the caller's
    /// problem — `WavesPreviewer::remove_status_def` / `remove_tag_def` clear
    /// them in the same undo step.
    pub fn remove(&mut self, id: &str) -> Option<LabelDef> {
        let index = self.position(id)?;
        Some(self.defs.remove(index))
    }

    /// Sort rank for an id: its position in the palette, or past the end for
    /// one this palette does not define.
    pub fn rank(&self, id: &str) -> usize {
        self.position(id).unwrap_or(self.defs.len())
    }

    fn unique_id(&self, base: &str) -> String {
        if self.get(base).is_none() {
            return base.to_string();
        }
        for n in 2..u32::MAX {
            let candidate = format!("{base}-{n}");
            if self.get(&candidate).is_none() {
                return candidate;
            }
        }
        base.to_string()
    }
}

/// Grey for an id no palette in this app defines.
pub const UNKNOWN_COLOR: [u8; 3] = [110, 116, 128];

/// Lowercase ASCII slug: letters, digits and `-`. Non-ASCII labels (Japanese,
/// say) have nothing to transliterate to, so they fall back to a short hash of
/// the label — still content-derived, still stable, still collision-checked by
/// `unique_id`.
pub fn slugify(label: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for ch in label.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out.truncate(48);
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        format!("label-{:08x}", fnv1a(label.trim()))
    } else {
        out
    }
}

fn fnv1a(text: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in text.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Palette a fresh install starts from, before the user edits anything.
pub fn default_status_palette() -> LabelPalette {
    let mut palette = LabelPalette::default();
    for (id, label, color) in [
        ("todo", "Todo", [110, 116, 128]),
        ("wip", "WIP", [212, 152, 56]),
        ("review", "Review", [78, 132, 210]),
        ("ok", "OK", [76, 160, 96]),
        ("ng", "NG", [196, 74, 74]),
    ] {
        palette.insert_stored(id, label, color);
    }
    palette
}

/// Tags start empty: unlike a status workflow there is no set that is right
/// for everybody, and an empty palette makes the Tags column visibly opt-in.
pub fn default_tag_palette() -> LabelPalette {
    LabelPalette::default()
}

/// One prefs line: `<slug>|<r>,<g>,<b>|<label>`. The label goes last and is
/// taken verbatim, so a label containing `|` or `,` survives.
pub fn encode_def(def: &LabelDef) -> String {
    format!(
        "{}|{},{},{}|{}",
        def.id, def.color[0], def.color[1], def.color[2], def.label
    )
}

pub fn decode_def(line: &str) -> Option<(String, String, [u8; 3])> {
    let mut parts = line.splitn(3, '|');
    let id = parts.next()?.trim().to_string();
    let color_text = parts.next()?;
    let label = parts.next().unwrap_or("").to_string();
    if id.is_empty() {
        return None;
    }
    let mut channels = color_text.split(',');
    let mut color = UNKNOWN_COLOR;
    for slot in color.iter_mut() {
        *slot = channels.next()?.trim().parse::<u8>().ok()?;
    }
    Some((id, label, color))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_makes_a_readable_ascii_id() {
        assert_eq!(slugify("Needs Review"), "needs-review");
        assert_eq!(slugify("  OK!  "), "ok");
        assert_eq!(slugify("a///b"), "a-b");
    }

    #[test]
    fn slugify_falls_back_to_a_stable_hash_for_a_non_ascii_label() {
        let first = slugify("確認待ち");
        // Nothing to transliterate, but the id must still be non-empty and
        // must not change between runs, or every save would re-key the rows.
        assert!(first.starts_with("label-"));
        assert_eq!(first, slugify("確認待ち"));
        assert_ne!(first, slugify("完了"));
    }

    #[test]
    fn two_labels_with_the_same_text_get_different_ids() {
        let mut palette = LabelPalette::default();
        let first = palette.add("Review", [0, 0, 0]);
        let second = palette.add("Review", [0, 0, 0]);
        assert_ne!(first, second);
        assert_eq!(&*first, "review");
        assert_eq!(&*second, "review-2");
    }

    #[test]
    fn renaming_a_label_keeps_the_id_rows_point_at() {
        let mut palette = default_status_palette();
        palette.rename("wip", "In Progress");
        assert_eq!(palette.label_for("wip"), "In Progress");
        assert!(palette.get("wip").is_some());
    }

    #[test]
    fn an_id_the_palette_does_not_define_keeps_its_raw_text() {
        let palette = default_status_palette();
        assert_eq!(
            palette.label_for("someone-elses-status"),
            "someone-elses-status"
        );
        assert_eq!(palette.color_for("someone-elses-status"), UNKNOWN_COLOR);
        // Past every defined entry, so unknown ids sort last rather than first.
        assert_eq!(palette.rank("someone-elses-status"), palette.len());
    }

    #[test]
    fn a_stored_definition_keeps_its_id_and_a_duplicate_is_skipped() {
        let mut palette = LabelPalette::default();
        assert!(palette.insert_stored("review", "Review", [1, 2, 3]));
        assert!(!palette.insert_stored("review", "Other", [4, 5, 6]));
        assert_eq!(palette.len(), 1);
        assert_eq!(palette.label_for("review"), "Review");
    }

    #[test]
    fn removing_a_definition_drops_it_from_the_palette() {
        let mut palette = default_status_palette();
        let before = palette.len();
        assert!(palette.remove("ng").is_some());
        assert_eq!(palette.len(), before - 1);
        assert!(palette.get("ng").is_none());
    }

    #[test]
    fn prefs_lines_round_trip_including_a_label_with_separators() {
        let def = LabelDef {
            id: Arc::from("review"),
            label: "needs a|second, look".to_string(),
            color: [10, 20, 30],
        };
        let (id, label, color) = decode_def(&encode_def(&def)).expect("decodes");
        assert_eq!(id, "review");
        assert_eq!(label, "needs a|second, look");
        assert_eq!(color, [10, 20, 30]);
    }

    #[test]
    fn a_malformed_prefs_line_is_rejected_rather_than_guessed_at() {
        assert!(decode_def("").is_none());
        assert!(decode_def("review").is_none());
        assert!(decode_def("review|1,2").is_none());
        assert!(decode_def("review|nope,2,3|Review").is_none());
        assert!(decode_def("|1,2,3|Review").is_none());
    }

    #[test]
    fn text_color_flips_with_the_fill_luminance() {
        assert_eq!(
            text_color_on([250, 250, 250]),
            egui::Color32::from_rgb(20, 22, 26)
        );
        assert_eq!(text_color_on([20, 20, 20]), egui::Color32::WHITE);
    }
}

/// Build a palette from a session's stored definitions, keeping their ids.
pub fn palette_from_project(defs: &[crate::app::project::ProjectLabelDef]) -> LabelPalette {
    let mut palette = LabelPalette::default();
    for def in defs {
        palette.insert_stored(&def.id, &def.label, def.color);
    }
    palette
}

pub fn palette_to_project(palette: &LabelPalette) -> Vec<crate::app::project::ProjectLabelDef> {
    palette
        .defs
        .iter()
        .map(|def| crate::app::project::ProjectLabelDef {
            id: def.id.to_string(),
            label: def.label.clone(),
            color: def.color,
        })
        .collect()
}

impl crate::app::WavesPreviewer {
    pub(crate) fn label_palette(&self, tags: bool) -> &LabelPalette {
        if tags {
            &self.tag_palette
        } else {
            &self.status_palette
        }
    }

    pub(crate) fn label_palette_mut(&mut self, tags: bool) -> &mut LabelPalette {
        if tags {
            &mut self.tag_palette
        } else {
            &mut self.status_palette
        }
    }

    /// How many rows carry each label. Shown next to the delete button so
    /// nobody removes a label without seeing what it costs.
    ///
    /// Counted in one pass over the list rather than one pass per definition:
    /// the manager window asks every frame it is open, and the list here can
    /// hold a hundred thousand rows.
    pub(crate) fn label_usage_counts(&self, tags: bool) -> std::collections::HashMap<&str, usize> {
        let mut counts = std::collections::HashMap::new();
        for item in &self.items {
            if tags {
                for tag in item.tags() {
                    *counts.entry(&**tag).or_insert(0) += 1;
                }
            } else if let Some(id) = item.status_id.as_deref() {
                *counts.entry(id).or_insert(0) += 1;
            }
        }
        counts
    }

    /// How many rows carry `id`. For a single lookup; use
    /// `label_usage_counts` when asking about a whole palette.
    pub(crate) fn label_usage_count(&self, tags: bool, id: &str) -> usize {
        if tags {
            self.items.iter().filter(|item| item.has_tag(id)).count()
        } else {
            self.items
                .iter()
                .filter(|item| item.status_id.as_deref() == Some(id))
                .count()
        }
    }

    /// The status all of `paths` share, or `None` when they disagree or none
    /// is set. Drives the check mark in the bulk menu.
    /// Set (or clear, with `None`) the status of every path in `paths`, as one
    /// undoable step.
    pub(crate) fn set_status_for_paths(&mut self, paths: &[std::path::PathBuf], id: Option<&str>) {
        let value = id.and_then(|id| {
            self.status_palette
                .interned(id)
                .or_else(|| Some(std::sync::Arc::from(id)))
        });
        self.edit_labels_for_paths(paths, |item| {
            if item.status_id.as_deref() == value.as_deref() {
                return false;
            }
            item.status_id = value.clone();
            true
        });
    }

    /// Add or remove one tag across every path in `paths`, as one undoable
    /// step. An id the palette does not define is still applied verbatim, so a
    /// bulk edit never depends on the palette being in sync first.
    pub(crate) fn set_tag_for_paths(&mut self, paths: &[std::path::PathBuf], id: &str, on: bool) {
        let value = self
            .tag_palette
            .interned(id)
            .unwrap_or_else(|| std::sync::Arc::from(id));
        self.edit_labels_for_paths(paths, |item| {
            if item.has_tag(&value) == on {
                return false;
            }
            item.set_tag(&value, on);
            true
        });
    }

    /// Delete a definition and strip it from every row that used it, in one
    /// undo step. Leaving the assignments behind would keep writing an id
    /// nothing defines into the session on every save.
    pub(crate) fn remove_label_def(&mut self, tags: bool, id: &str) {
        let paths: Vec<std::path::PathBuf> = self
            .items
            .iter()
            .filter(|item| {
                if tags {
                    item.has_tag(id)
                } else {
                    item.status_id.as_deref() == Some(id)
                }
            })
            .map(|item| item.path.clone())
            .collect();
        if tags {
            self.set_tag_for_paths(&paths, id, false);
        } else {
            self.set_status_for_paths(&paths, None);
        }
        self.label_palette_mut(tags).remove(id);
        if !tags && self.default_status.as_deref() == Some(id) {
            self.default_status = None;
        }
        self.save_label_palette_prefs();
    }

    /// Persist a palette edit.
    ///
    /// Only when no session is open: with one open the palette belongs to
    /// that session and is written on its save, and pushing it to prefs would
    /// silently replace this machine's own set with a colleague's the moment
    /// you renamed one label in their shared session. The manager window's
    /// "Save as global default" is the deliberate way to do that. The List
    /// Columns window position is gated on the same condition.
    pub(crate) fn save_label_palette_prefs(&self) {
        if self.project_path.is_none() {
            self.save_prefs();
        }
    }

    /// Apply `edit` to each path and record one list-undo action covering the
    /// whole batch. Rows `edit` reports unchanged are still captured, so undo
    /// restores a mixed selection exactly.
    fn edit_labels_for_paths(
        &mut self,
        paths: &[std::path::PathBuf],
        mut edit: impl FnMut(&mut crate::app::types::MediaItem) -> bool,
    ) {
        if paths.is_empty() {
            return;
        }
        let before = self.capture_list_selection_snapshot();
        let before_items = self.capture_list_undo_items_by_paths(paths);
        let mut changed = false;
        for path in paths {
            if let Some(item) = self.item_for_path_mut(path) {
                changed |= edit(item);
            }
        }
        if !changed {
            return;
        }
        self.record_list_update_from_paths(paths, before_items, before);
    }

    /// Replace a palette wholesale (session open, or "load global default")
    /// and re-intern every row's ids against it, so rows keep sharing one
    /// allocation per label instead of drifting into one string per row.
    pub(crate) fn adopt_palette(&mut self, tags: bool, palette: LabelPalette) {
        *self.label_palette_mut(tags) = palette;
        self.reintern_label_ids(tags);
    }

    fn reintern_label_ids(&mut self, tags: bool) {
        let palette = self.label_palette(tags).clone();
        if tags {
            for item in &mut self.items {
                if item.tags.is_none() {
                    continue;
                }
                let reinterned: Vec<std::sync::Arc<str>> = item
                    .tags()
                    .iter()
                    .map(|id| palette.interned(id).unwrap_or_else(|| id.clone()))
                    .collect();
                item.set_tags(reinterned);
            }
        } else {
            for item in &mut self.items {
                if let Some(id) = item.status_id.as_ref() {
                    if let Some(interned) = palette.interned(id) {
                        item.status_id = Some(interned);
                    }
                }
            }
        }
    }

    /// Make sure every id the rows use resolves to something. Called after a
    /// session is applied, so a `.nwsess` that assigns a label its own palette
    /// block omitted still shows (and re-saves) that label.
    pub(crate) fn ensure_label_defs_for_rows(&mut self) {
        let status_ids: Vec<String> = self
            .items
            .iter()
            .filter_map(|item| item.status_id.as_deref().map(str::to_string))
            .collect();
        for id in status_ids {
            self.status_palette.ensure_placeholder(&id);
        }
        let tag_ids: Vec<String> = self
            .items
            .iter()
            .flat_map(|item| item.tags().iter().map(|id| id.to_string()))
            .collect();
        for id in tag_ids {
            self.tag_palette.ensure_placeholder(&id);
        }
        self.reintern_label_ids(false);
        self.reintern_label_ids(true);
    }

    /// Clear every row's status and tags. Session restore runs this before
    /// applying the stored assignments: the rows were built through
    /// `make_media_item`, which stamps the default status, and without this a
    /// row the user deliberately set back to "no status" would come back
    /// wearing the default on every reopen.
    #[allow(dead_code)]
    pub(crate) fn clear_all_row_labels(&mut self) {
        for item in &mut self.items {
            item.status_id = None;
            item.tags = None;
        }
    }
}
