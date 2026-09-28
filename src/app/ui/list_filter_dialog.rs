//! The column filter dialog: Excel's AutoFilter as a modal.
//!
//! Two ways to filter a column, as in Excel: tick values in the column's
//! value list, or set up to two conditions joined by AND / OR. The value list
//! shows what the column holds among the rows the *other* filters let
//! through, gathered a slice per frame so a large list never stalls the UI.

use std::collections::{HashMap, HashSet};

use egui::RichText;

use crate::app::list_filter::{
    display_text, ColumnFilter, ColumnValueKind, CompiledColumnFilter, Condition,
    ConditionOp, FilterError, FilterRule, Join, ValueSelection,
};
use crate::app::sort_filter_jobs::CompiledListFilter;
use crate::app::types::{MediaId, SortKey};

/// Excel shows at most this many distinct values in a filter list; past it
/// a column is better filtered by condition than by ticking.
pub(crate) const VALUE_LIST_LIMIT: usize = 10_000;
/// Rows scanned between clock checks while gathering the value list.
const VALUE_SCAN_CHUNK: usize = 1_024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilterTab {
    Values,
    Conditions,
}

pub(crate) struct ConditionDraft {
    pub op: ConditionOp,
    pub a: String,
    pub b: String,
}

impl ConditionDraft {
    fn new(op: ConditionOp) -> Self {
        Self { op, a: String::new(), b: String::new() }
    }

    fn from(c: &Condition) -> Self {
        Self { op: c.op, a: c.a.clone(), b: c.b.clone() }
    }

    fn to_condition(&self) -> Condition {
        Condition { op: self.op, a: self.a.clone(), b: self.b.clone() }
    }
}

/// One distinct value in the column: what it reads, how many rows hold it,
/// and a number to order it by when it is one.
struct ValueEntry {
    text: String,
    rows: usize,
    number: Option<f64>,
}

struct ValueScan {
    filter: CompiledListFilter,
    ids: Vec<MediaId>,
    cursor: usize,
    counts: HashMap<String, (usize, Option<f64>)>,
    blank_rows: usize,
}

pub(crate) struct ListFilterDialog {
    pub key: SortKey,
    pub label: String,
    pub kind: ColumnValueKind,
    pub tab: FilterTab,
    pub first: ConditionDraft,
    pub use_second: bool,
    pub join: Join,
    pub second: ConditionDraft,
    pub error: Option<FilterError>,
    // Value list
    entries: Vec<ValueEntry>,
    truncated: bool,
    blank_rows: usize,
    checked: HashSet<String>,
    blank_checked: bool,
    /// Tick every value once the scan lists them (no value filter yet).
    check_all_when_listed: bool,
    search: String,
    /// Indices into `entries` matching `search`, and the text they match.
    visible: Vec<usize>,
    visible_for: Option<String>,
    scan: Option<ValueScan>,
}

impl crate::app::WavesPreviewer {
    /// Opens the filter dialog for a column, seeded with its current filter.
    pub(in crate::app) fn open_list_filter_dialog(&mut self, key: SortKey, label: &str) {
        let existing = self.column_filters.iter().find(|f| f.key == key).cloned();
        let kind = existing
            .as_ref()
            .map(|f| f.kind)
            .unwrap_or_else(|| self.column_value_kind(key));
        let mut dialog = ListFilterDialog {
            key,
            label: label.to_string(),
            kind,
            tab: FilterTab::Values,
            first: ConditionDraft::new(kind.default_op()),
            use_second: false,
            join: Join::And,
            second: ConditionDraft::new(kind.default_op()),
            error: None,
            entries: Vec::new(),
            truncated: false,
            blank_rows: 0,
            checked: HashSet::new(),
            blank_checked: true,
            check_all_when_listed: true,
            search: String::new(),
            visible: Vec::new(),
            visible_for: None,
            scan: None,
        };
        match existing.map(|f| f.rule) {
            Some(FilterRule::Values(sel)) => {
                dialog.checked = sel.values.into_iter().collect();
                dialog.blank_checked = sel.blank;
                dialog.check_all_when_listed = false;
            }
            Some(FilterRule::Conditions { first, second }) => {
                dialog.tab = FilterTab::Conditions;
                dialog.first = ConditionDraft::from(&first);
                if let Some((join, cond)) = second {
                    dialog.use_second = true;
                    dialog.join = join;
                    dialog.second = ConditionDraft::from(&cond);
                }
            }
            None => {}
        }
        // The value list reflects the rows every *other* filter lets through.
        let filter = self.compile_list_filter().without_column(key);
        dialog.scan = Some(ValueScan {
            filter,
            ids: self.items.iter().map(|item| item.id).collect(),
            cursor: 0,
            counts: HashMap::new(),
            blank_rows: 0,
        });
        self.list_filter_dialog = Some(dialog);
        if key.depends_on_metadata() {
            // A value list over unread metadata would be mostly blanks.
            self.prime_sort_metadata_prefetch();
        }
    }

    /// Replaces (or with `None` removes) the filter on one column and
    /// re-filters the list.
    pub(in crate::app) fn set_column_filter(&mut self, key: SortKey, filter: Option<ColumnFilter>) {
        self.column_filters.retain(|f| f.key != key);
        if let Some(filter) = filter {
            if filter.key.depends_on_metadata() {
                self.prime_sort_metadata_prefetch();
            }
            self.column_filters.push(filter);
        }
        self.refresh_filter_then_sort();
    }

    pub(in crate::app) fn clear_all_column_filters(&mut self) {
        if self.column_filters.is_empty() {
            return;
        }
        self.column_filters.clear();
        self.refresh_filter_then_sort();
    }

    /// Gathers a slice of the value list within the frame budget.
    fn pump_list_filter_value_scan(&mut self) -> bool {
        let Some(mut dialog) = self.list_filter_dialog.take() else {
            return false;
        };
        let busy = if let Some(mut scan) = dialog.scan.take() {
            let key = dialog.key;
            let kind = dialog.kind;
            let started = std::time::Instant::now();
            let budget_ms = self.perf.list_job_frame_budget_ms();
            while scan.cursor < scan.ids.len()
                && started.elapsed().as_secs_f64() * 1000.0 < budget_ms
            {
                let end = (scan.cursor + VALUE_SCAN_CHUNK).min(scan.ids.len());
                for &id in &scan.ids[scan.cursor..end] {
                    let Some(item) = self.item_for_id(id) else {
                        continue;
                    };
                    if !self.row_passes(item, &scan.filter, None) {
                        continue;
                    }
                    let value = self.column_value(item, key);
                    let number = value.number();
                    let texts: Vec<String> = display_text(kind, &value)
                        .into_iter()
                        .filter(|text| !text.is_empty())
                        .collect();
                    if texts.is_empty() {
                        scan.blank_rows += 1;
                    }
                    for text in texts {
                        scan.counts.entry(text).or_insert((0, number)).0 += 1;
                    }
                }
                scan.cursor = end;
            }
            if scan.cursor < scan.ids.len() {
                dialog.scan = Some(scan);
                true
            } else {
                dialog.finish_scan(scan);
                false
            }
        } else {
            false
        };
        self.list_filter_dialog = Some(dialog);
        busy
    }

    /// Draws the dialog while it is open.
    pub(in crate::app) fn ui_list_filter_dialog(&mut self, ctx: &egui::Context) {
        if self.list_filter_dialog.is_none() {
            return;
        }
        if self.pump_list_filter_value_scan() {
            ctx.request_repaint();
        }
        let mut outcome = DialogOutcome::Open;
        let Some(mut dialog) = self.list_filter_dialog.take() else {
            return;
        };
        let modal = egui::Modal::new(egui::Id::new("list_column_filter_dialog")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading(format!("Filter: {}", dialog.label));
            ui.label(RichText::new(kind_caption(dialog.kind)).weak().small());
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut dialog.tab, FilterTab::Values, "Values");
                ui.selectable_value(&mut dialog.tab, FilterTab::Conditions, "Conditions");
            });
            ui.separator();
            match dialog.tab {
                FilterTab::Values => dialog.ui_values(ui),
                FilterTab::Conditions => dialog.ui_conditions(ui),
            }
            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Clear filter").clicked() {
                    outcome = DialogOutcome::Clear;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        outcome = DialogOutcome::Cancel;
                    }
                    if ui
                        .add(egui::Button::new(RichText::new("OK").strong()))
                        .clicked()
                    {
                        outcome = DialogOutcome::Apply;
                    }
                });
            });
        });
        if modal.should_close() && matches!(outcome, DialogOutcome::Open) {
            outcome = DialogOutcome::Cancel;
        }
        match outcome {
            DialogOutcome::Open => self.list_filter_dialog = Some(dialog),
            DialogOutcome::Cancel => {}
            DialogOutcome::Clear => self.set_column_filter(dialog.key, None),
            DialogOutcome::Apply => match dialog.build_filter() {
                Ok(filter) => self.set_column_filter(dialog.key, filter),
                Err(error) => {
                    dialog.error = Some(error);
                    self.list_filter_dialog = Some(dialog);
                }
            },
        }
    }
}

enum DialogOutcome {
    Open,
    Cancel,
    Clear,
    Apply,
}

fn kind_caption(kind: ColumnValueKind) -> &'static str {
    match kind {
        ColumnValueKind::Text => "Text column",
        ColumnValueKind::Number => "Number column",
        ColumnValueKind::Duration => "Time column (m:ss.s)",
        ColumnValueKind::DateTime => "Date column",
    }
}

#[cfg(feature = "kittest")]
impl ListFilterDialog {
    pub(crate) fn test_values(&self) -> Option<Vec<(String, usize)>> {
        if self.scan.is_some() {
            return None;
        }
        Some(self.entries.iter().map(|e| (e.text.clone(), e.rows)).collect())
    }

    pub(crate) fn test_check_only(&mut self, values: &[&str]) {
        self.tab = FilterTab::Values;
        self.checked = values.iter().map(|v| v.to_string()).collect();
        self.blank_checked = false;
    }

    pub(crate) fn test_use_condition(&mut self, op_label: &str, a: &str) -> bool {
        let Some(op) = self.kind.ops().into_iter().find(|op| op.label() == op_label) else {
            return false;
        };
        self.tab = FilterTab::Conditions;
        self.first = ConditionDraft { op, a: a.to_string(), b: String::new() };
        self.use_second = false;
        true
    }
}

impl ListFilterDialog {
    fn finish_scan(&mut self, scan: ValueScan) {
        let mut entries: Vec<ValueEntry> = scan
            .counts
            .into_iter()
            .map(|(text, (rows, number))| ValueEntry { text, rows, number })
            .collect();
        // Numbers in numeric order (and dates, durations), text naturally.
        entries.sort_by(|a, b| match (a.number, b.number) {
            (Some(x), Some(y)) => x.total_cmp(&y),
            _ => a.text.to_lowercase().cmp(&b.text.to_lowercase()),
        });
        self.truncated = entries.len() > VALUE_LIST_LIMIT;
        entries.truncate(VALUE_LIST_LIMIT);
        if self.check_all_when_listed {
            self.checked = entries.iter().map(|e| e.text.clone()).collect();
            self.blank_checked = true;
        }
        self.blank_rows = scan.blank_rows;
        self.entries = entries;
        self.visible_for = None;
    }

    fn refresh_visible(&mut self) {
        if self.visible_for.as_deref() == Some(self.search.as_str()) {
            return;
        }
        let needle = self.search.trim().to_lowercase();
        self.visible = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                needle.is_empty() || crate::app::text_match::contains_ignore_case(&e.text, &needle)
            })
            .map(|(i, _)| i)
            .collect();
        self.visible_for = Some(self.search.clone());
    }

    fn ui_values(&mut self, ui: &mut egui::Ui) {
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text("Search values")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(4.0);
        if self.scan.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Collecting values…");
            });
            return;
        }
        self.refresh_visible();
        let searching = !self.search.trim().is_empty();
        // "(Select all)" toggles what is listed -- with a search, only the
        // search results, as in Excel.
        let all_on = self
            .visible
            .iter()
            .all(|&i| self.checked.contains(&self.entries[i].text))
            && (searching || self.blank_checked || self.blank_rows == 0);
        let mut all = all_on;
        let all_label = if searching {
            "(Select all search results)"
        } else {
            "(Select all)"
        };
        if ui.checkbox(&mut all, all_label).changed() {
            for &i in &self.visible {
                let text = &self.entries[i].text;
                if all {
                    self.checked.insert(text.clone());
                } else {
                    self.checked.remove(text);
                }
            }
            if !searching {
                self.blank_checked = all;
            }
        }
        /// Row height of the value list, in points.
        const VALUE_ROW_H: f32 = 20.0;
        /// Height of the value list's scroll area, in points.
        const VALUE_LIST_H: f32 = 240.0;
        egui::ScrollArea::vertical()
            .id_salt("list_filter_values")
            .max_height(VALUE_LIST_H)
            .auto_shrink([false, true])
            .show_rows(ui, VALUE_ROW_H, self.visible.len(), |ui, range| {
                for &i in &self.visible[range] {
                    let entry = &self.entries[i];
                    let mut on = self.checked.contains(&entry.text);
                    let label = format!("{}  ({})", entry.text, entry.rows);
                    if ui.checkbox(&mut on, label).changed() {
                        if on {
                            self.checked.insert(entry.text.clone());
                        } else {
                            self.checked.remove(&entry.text);
                        }
                    }
                }
            });
        if self.blank_rows > 0 && !searching {
            ui.checkbox(
                &mut self.blank_checked,
                format!("(Blanks)  ({})", self.blank_rows),
            );
        }
        if self.truncated {
            ui.label(
                RichText::new(format!(
                    "Only the first {VALUE_LIST_LIMIT} values are listed; use Conditions for the rest."
                ))
                .weak()
                .small(),
            );
        }
    }

    fn ui_conditions(&mut self, ui: &mut egui::Ui) {
        let kind = self.kind;
        let error_for = |i: usize, error: &Option<FilterError>| {
            error.as_ref().filter(|e| e.condition == i).map(|e| e.message.clone())
        };
        let first_error = error_for(0, &self.error);
        condition_row(ui, "first", kind, &mut self.first, first_error.as_deref());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.use_second, "and a second condition:");
            ui.add_enabled_ui(self.use_second, |ui| {
                ui.radio_value(&mut self.join, Join::And, "AND");
                ui.radio_value(&mut self.join, Join::Or, "OR");
            });
        });
        if self.use_second {
            let second_error = error_for(1, &self.error);
            condition_row(ui, "second", kind, &mut self.second, second_error.as_deref());
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new(match kind {
                ColumnValueKind::Text => "Case is ignored. In \"equals\", * matches any text and ? one character.",
                ColumnValueKind::Duration => "Type times as 1:23.5 or 83.5 (seconds).",
                ColumnValueKind::DateTime => "Type dates as 2026-09-27, optionally with a time.",
                ColumnValueKind::Number => "Rows with no value count as blank.",
            })
            .weak()
            .small(),
        );
    }

    /// The filter to set, or `None` when the dialog asks for no filtering
    /// (every value ticked). Conditions are compiled here so a mistake is
    /// reported against its field instead of silently matching nothing.
    fn build_filter(&self) -> Result<Option<ColumnFilter>, FilterError> {
        let rule = match self.tab {
            FilterTab::Values => {
                if self.scan.is_some() {
                    return Err(FilterError {
                        condition: 0,
                        message: "still collecting values".into(),
                    });
                }
                let everything = self.entries.iter().all(|e| self.checked.contains(&e.text))
                    && (self.blank_checked || self.blank_rows == 0)
                    && !self.truncated;
                if everything {
                    return Ok(None);
                }
                FilterRule::Values(ValueSelection {
                    values: self.checked.iter().cloned().collect(),
                    blank: self.blank_checked,
                })
            }
            FilterTab::Conditions => FilterRule::Conditions {
                first: self.first.to_condition(),
                second: self
                    .use_second
                    .then(|| (self.join, self.second.to_condition())),
            },
        };
        CompiledColumnFilter::compile(self.kind, &rule, chrono::Local::now())?;
        Ok(Some(ColumnFilter { key: self.key, kind: self.kind, rule }))
    }
}

fn condition_row(
    ui: &mut egui::Ui,
    id: &str,
    kind: ColumnValueKind,
    draft: &mut ConditionDraft,
    error: Option<&str>,
) {
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(("list_filter_op", id))
            .width(150.0)
            .selected_text(draft.op.label())
            .show_ui(ui, |ui| {
                for op in kind.ops() {
                    ui.selectable_value(&mut draft.op, op, op.label());
                }
            });
        let operands = draft.op.operand_count();
        if operands >= 1 {
            ui.add(
                egui::TextEdit::singleline(&mut draft.a)
                    .hint_text(operand_hint(kind, draft.op))
                    .desired_width(if operands == 2 { 110.0 } else { 230.0 }),
            );
        }
        if operands == 2 {
            ui.label("and");
            ui.add(
                egui::TextEdit::singleline(&mut draft.b)
                    .hint_text(operand_hint(kind, draft.op))
                    .desired_width(110.0),
            );
        }
    });
    if let Some(error) = error {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }
}

fn operand_hint(kind: ColumnValueKind, op: ConditionOp) -> &'static str {
    use crate::app::list_filter::{DateOp, NumberOp};
    match op {
        ConditionOp::Number(NumberOp::TopItems | NumberOp::BottomItems) => "count",
        ConditionOp::Number(NumberOp::TopPercent | NumberOp::BottomPercent) => "percent",
        ConditionOp::Date(DateOp::LastNDays) => "days",
        _ => kind.operand_hint(),
    }
}
