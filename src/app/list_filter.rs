//! Column filters for the file list, modelled on Excel's AutoFilter.
//!
//! A filter on a column is either a pick from the column's values (the
//! checkbox list) or up to two conditions joined by AND/OR (Excel's "Custom
//! AutoFilter"). Which conditions a column offers follows the kind of value
//! it holds: text, number, duration or date.
//!
//! This module is the rules only -- no UI, no `WavesPreviewer`. The list
//! turns a row into a [`CellValue`] (`column_value` in `sort_filter_jobs.rs`,
//! the same accessor sorting uses) and asks a [`CompiledColumnFilter`].
//! Conditions keep the text the user typed; they are parsed when compiled, so
//! a session stores exactly what was entered and a bad value can be reported
//! against the field it came from.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashSet};

use chrono::{Datelike, Duration as ChronoDuration, Local, NaiveDate, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};

use super::text_match::{contains_ignore_case, TextMatcher};

/// What a column holds, which decides the conditions it offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColumnValueKind {
    Text,
    Number,
    /// Seconds; typed and shown as `m:ss.s`.
    Duration,
    /// Seconds since the Unix epoch; typed and shown as local date and time.
    DateTime,
}

/// One cell, as filtering and sorting see it.
#[derive(Clone, Debug, PartialEq)]
pub enum CellValue<'a> {
    Text(Cow<'a, str>),
    /// Several values in one cell (a row's tags). A condition holds if it
    /// holds for any of them; a negative one ("does not contain") if it holds
    /// for all.
    Texts(Vec<Cow<'a, str>>),
    Number(f64),
    /// Not known (metadata not read yet) or not applicable: Excel's blank.
    Missing,
}

impl CellValue<'_> {
    fn is_blank(&self) -> bool {
        match self {
            Self::Missing => true,
            Self::Text(t) => t.trim().is_empty(),
            Self::Texts(v) => v.iter().all(|t| t.trim().is_empty()),
            Self::Number(n) => !n.is_finite(),
        }
    }

    pub fn number(&self) -> Option<f64> {
        match self {
            Self::Number(n) if n.is_finite() => Some(*n),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// What the user sets up (stored in the session as-is)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextOp {
    Equals,
    NotEquals,
    BeginsWith,
    EndsWith,
    Contains,
    NotContains,
    MatchesRegex,
    Blank,
    NotBlank,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NumberOp {
    Equals,
    NotEquals,
    Greater,
    GreaterOrEqual,
    Less,
    LessOrEqual,
    Between,
    TopItems,
    BottomItems,
    TopPercent,
    BottomPercent,
    AboveAverage,
    BelowAverage,
    Blank,
    NotBlank,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DateOp {
    On,
    Before,
    After,
    Between,
    Today,
    Yesterday,
    ThisWeek,
    LastWeek,
    ThisMonth,
    LastMonth,
    ThisYear,
    LastNDays,
    Blank,
    NotBlank,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConditionOp {
    Text(TextOp),
    Number(NumberOp),
    Date(DateOp),
}

/// One condition: an operator and up to two typed operands (the second only
/// for "between").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    pub op: ConditionOp,
    #[serde(default)]
    pub a: String,
    #[serde(default)]
    pub b: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Join {
    #[default]
    And,
    Or,
}

/// The checkbox list: which values stay visible.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct ValueSelection {
    pub values: BTreeSet<String>,
    /// Whether "(Blanks)" is ticked.
    #[serde(default)]
    pub blank: bool,
}

/// A filter set on one list column. `kind` is recorded with it: a metadata
/// column is typed by its values, and the rule's operands must be read the
/// same way when the session is opened again.
#[derive(Clone, Debug, PartialEq)]
pub struct ColumnFilter {
    pub key: crate::app::types::SortKey,
    pub kind: ColumnValueKind,
    pub rule: FilterRule,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FilterRule {
    Values(ValueSelection),
    Conditions {
        first: Condition,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        second: Option<(Join, Condition)>,
    },
}

// ---------------------------------------------------------------------------
// Operator lists and labels for the dialog
// ---------------------------------------------------------------------------

impl ColumnValueKind {
    /// The operators a column of this kind offers, in Excel's order.
    pub fn ops(self) -> Vec<ConditionOp> {
        match self {
            Self::Text => [
                TextOp::Equals,
                TextOp::NotEquals,
                TextOp::BeginsWith,
                TextOp::EndsWith,
                TextOp::Contains,
                TextOp::NotContains,
                TextOp::MatchesRegex,
                TextOp::Blank,
                TextOp::NotBlank,
            ]
            .into_iter()
            .map(ConditionOp::Text)
            .collect(),
            Self::Number | Self::Duration => [
                NumberOp::Equals,
                NumberOp::NotEquals,
                NumberOp::Greater,
                NumberOp::GreaterOrEqual,
                NumberOp::Less,
                NumberOp::LessOrEqual,
                NumberOp::Between,
                NumberOp::TopItems,
                NumberOp::BottomItems,
                NumberOp::TopPercent,
                NumberOp::BottomPercent,
                NumberOp::AboveAverage,
                NumberOp::BelowAverage,
                NumberOp::Blank,
                NumberOp::NotBlank,
            ]
            .into_iter()
            .map(ConditionOp::Number)
            .collect(),
            Self::DateTime => [
                DateOp::On,
                DateOp::Before,
                DateOp::After,
                DateOp::Between,
                DateOp::Today,
                DateOp::Yesterday,
                DateOp::ThisWeek,
                DateOp::LastWeek,
                DateOp::ThisMonth,
                DateOp::LastMonth,
                DateOp::ThisYear,
                DateOp::LastNDays,
                DateOp::Blank,
                DateOp::NotBlank,
            ]
            .into_iter()
            .map(ConditionOp::Date)
            .collect(),
        }
    }

    pub fn default_op(self) -> ConditionOp {
        match self {
            Self::Text => ConditionOp::Text(TextOp::Contains),
            Self::Number | Self::Duration => ConditionOp::Number(NumberOp::GreaterOrEqual),
            Self::DateTime => ConditionOp::Date(DateOp::After),
        }
    }

    /// What to type into an operand field, shown as its hint.
    pub fn operand_hint(self) -> &'static str {
        match self {
            Self::Text => "text  (* and ? are wildcards in equals)",
            Self::Number => "number",
            Self::Duration => "time: 1:23.5 or 83.5",
            Self::DateTime => "date: 2026-09-27 or 2026-09-27 14:30",
        }
    }
}

impl ConditionOp {
    pub fn label(self) -> &'static str {
        match self {
            Self::Text(op) => match op {
                TextOp::Equals => "equals",
                TextOp::NotEquals => "does not equal",
                TextOp::BeginsWith => "begins with",
                TextOp::EndsWith => "ends with",
                TextOp::Contains => "contains",
                TextOp::NotContains => "does not contain",
                TextOp::MatchesRegex => "matches regex",
                TextOp::Blank => "is blank",
                TextOp::NotBlank => "is not blank",
            },
            Self::Number(op) => match op {
                NumberOp::Equals => "=",
                NumberOp::NotEquals => "≠",
                NumberOp::Greater => ">",
                NumberOp::GreaterOrEqual => "≥",
                NumberOp::Less => "<",
                NumberOp::LessOrEqual => "≤",
                NumberOp::Between => "between",
                NumberOp::TopItems => "top N items",
                NumberOp::BottomItems => "bottom N items",
                NumberOp::TopPercent => "top N %",
                NumberOp::BottomPercent => "bottom N %",
                NumberOp::AboveAverage => "above average",
                NumberOp::BelowAverage => "below average",
                NumberOp::Blank => "is blank",
                NumberOp::NotBlank => "is not blank",
            },
            Self::Date(op) => match op {
                DateOp::On => "on",
                DateOp::Before => "before",
                DateOp::After => "after",
                DateOp::Between => "between",
                DateOp::Today => "today",
                DateOp::Yesterday => "yesterday",
                DateOp::ThisWeek => "this week",
                DateOp::LastWeek => "last week",
                DateOp::ThisMonth => "this month",
                DateOp::LastMonth => "last month",
                DateOp::ThisYear => "this year",
                DateOp::LastNDays => "in the last N days",
                DateOp::Blank => "is blank",
                DateOp::NotBlank => "is not blank",
            },
        }
    }

    /// How many operands the operator reads: 0, 1 or 2.
    pub fn operand_count(self) -> usize {
        match self {
            Self::Text(TextOp::Blank | TextOp::NotBlank)
            | Self::Number(
                NumberOp::AboveAverage | NumberOp::BelowAverage | NumberOp::Blank | NumberOp::NotBlank,
            )
            | Self::Date(
                DateOp::Today
                | DateOp::Yesterday
                | DateOp::ThisWeek
                | DateOp::LastWeek
                | DateOp::ThisMonth
                | DateOp::LastMonth
                | DateOp::ThisYear
                | DateOp::Blank
                | DateOp::NotBlank,
            ) => 0,
            Self::Number(NumberOp::Between) | Self::Date(DateOp::Between) => 2,
            _ => 1,
        }
    }

    /// Operators whose meaning depends on the whole column (top N, average).
    pub fn needs_aggregate(self) -> bool {
        matches!(
            self,
            Self::Number(
                NumberOp::TopItems
                    | NumberOp::BottomItems
                    | NumberOp::TopPercent
                    | NumberOp::BottomPercent
                    | NumberOp::AboveAverage
                    | NumberOp::BelowAverage
            )
        )
    }
}

// ---------------------------------------------------------------------------
// Parsing and formatting operands
// ---------------------------------------------------------------------------

/// `83.5`, `83.5s`, `1:23.5`, `1:02:03.25` -> seconds.
pub fn parse_duration_secs(text: &str) -> Option<f64> {
    let text = text.trim().trim_end_matches(['s', 'S']).trim();
    if text.is_empty() {
        return None;
    }
    let mut secs = 0.0;
    for part in text.split(':') {
        let v: f64 = part.trim().parse().ok()?;
        if !v.is_finite() || v < 0.0 {
            return None;
        }
        secs = secs * 60.0 + v;
    }
    Some(secs)
}

/// `2026-09-27`, `2026-09-27 14:30`, `2026-09-27 14:30:05` (also `/`) in
/// local time -> seconds since the epoch.
pub fn parse_local_datetime(text: &str) -> Option<f64> {
    let text = text.trim().replace('/', "-");
    let naive = NaiveDateTime::parse_from_str(&text, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(&text, "%Y-%m-%d %H:%M"))
        .or_else(|_| {
            NaiveDate::parse_from_str(&text, "%Y-%m-%d")
                .map(|d| d.and_hms_opt(0, 0, 0).unwrap_or_default())
        })
        .ok()?;
    let local = Local.from_local_datetime(&naive).earliest()?;
    Some(local.timestamp() as f64)
}

pub fn format_local_datetime(secs: f64) -> String {
    Local
        .timestamp_opt(secs.floor() as i64, 0)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// A cell as the value list shows it (and matches it). Built by one function
/// for both, so a ticked value always finds its rows again.
pub fn display_text(kind: ColumnValueKind, value: &CellValue) -> Vec<String> {
    match value {
        CellValue::Missing => Vec::new(),
        CellValue::Text(t) => vec![t.trim().to_string()],
        CellValue::Texts(v) => v.iter().map(|t| t.trim().to_string()).collect(),
        CellValue::Number(n) if !n.is_finite() => Vec::new(),
        CellValue::Number(n) => vec![match kind {
            ColumnValueKind::Duration => crate::app::helpers::format_duration(*n as f32),
            ColumnValueKind::DateTime => format_local_datetime(*n),
            _ => format_number(*n),
        }],
    }
}

/// Integers without a decimal point, everything else to one decimal --
/// enough to tell -14.0 from -14.3 LUFS without listing float noise.
pub fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n:.1}")
    }
}

// ---------------------------------------------------------------------------
// Compiled form
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct FilterError {
    /// 0 for the first condition, 1 for the second.
    pub condition: usize,
    pub message: String,
}

#[derive(Clone, Debug)]
enum TextTest {
    Equals(TextMatcher),
    Contains(String),
    BeginsWith(String),
    EndsWith(String),
    Regex(TextMatcher),
}

#[derive(Clone, Debug)]
enum Compiled {
    Blank,
    NotBlank,
    /// Holds when the text test holds (negated: when it holds for none).
    Text { test: TextTest, negate: bool },
    /// `lo` to `hi`, each end inclusive or not; an open end is an infinity.
    Range { lo: f64, lo_incl: bool, hi: f64, hi_incl: bool },
    NotEqual(f64),
    /// Top/bottom N and average: resolved against the column's values into a
    /// `Range` before use; `None` until then.
    Aggregate { op: NumberOp, n: f64, resolved: Option<Box<Compiled>> },
}

/// A column filter ready to test rows.
#[derive(Clone, Debug)]
pub struct CompiledColumnFilter {
    pub kind: ColumnValueKind,
    rule: CompiledRule,
}

#[derive(Clone, Debug)]
enum CompiledRule {
    Values { values: HashSet<String>, blank: bool },
    Conditions { first: Compiled, second: Option<(Join, Compiled)> },
}

/// Equality tolerance for numbers typed with fewer decimals than stored.
const NUMBER_EQ_EPSILON: f64 = 1e-6;

impl CompiledColumnFilter {
    /// Compiles a rule. `now` anchors "today" / "last week"; passed in so a
    /// test can pin it.
    pub fn compile(
        kind: ColumnValueKind,
        rule: &FilterRule,
        now: chrono::DateTime<Local>,
    ) -> Result<Self, FilterError> {
        let rule = match rule {
            FilterRule::Values(sel) => CompiledRule::Values {
                values: sel.values.iter().cloned().collect(),
                blank: sel.blank,
            },
            FilterRule::Conditions { first, second } => CompiledRule::Conditions {
                first: compile_condition(kind, first, now).map_err(|message| FilterError {
                    condition: 0,
                    message,
                })?,
                second: match second {
                    Some((join, cond)) => Some((
                        *join,
                        compile_condition(kind, cond, now).map_err(|message| FilterError {
                            condition: 1,
                            message,
                        })?,
                    )),
                    None => None,
                },
            },
        };
        Ok(Self { kind, rule })
    }

    pub fn needs_aggregate(&self) -> bool {
        match &self.rule {
            CompiledRule::Values { .. } => false,
            CompiledRule::Conditions { first, second } => {
                unresolved(first) || second.as_ref().is_some_and(|(_, c)| unresolved(c))
            }
        }
    }

    /// Resolves top/bottom N and average against the numbers the column
    /// holds (the rows that pass every *other* filter, as Excel does).
    pub fn resolve_aggregates(&mut self, values: &[f64]) {
        if let CompiledRule::Conditions { first, second } = &mut self.rule {
            resolve(first, values);
            if let Some((_, c)) = second {
                resolve(c, values);
            }
        }
    }

    pub fn matches(&self, value: &CellValue) -> bool {
        match &self.rule {
            CompiledRule::Values { values, blank } => {
                if value.is_blank() {
                    return *blank;
                }
                display_text(self.kind, value)
                    .iter()
                    .any(|text| values.contains(text))
            }
            CompiledRule::Conditions { first, second } => {
                let a = eval(first, value);
                match second {
                    None => a,
                    Some((Join::And, c)) => a && eval(c, value),
                    Some((Join::Or, c)) => a || eval(c, value),
                }
            }
        }
    }
}

fn at_least(lo: f64) -> Compiled {
    Compiled::Range { lo, lo_incl: true, hi: f64::INFINITY, hi_incl: true }
}
fn more_than(lo: f64) -> Compiled {
    Compiled::Range { lo, lo_incl: false, hi: f64::INFINITY, hi_incl: true }
}
fn at_most(hi: f64) -> Compiled {
    Compiled::Range { lo: f64::NEG_INFINITY, lo_incl: true, hi, hi_incl: true }
}
fn less_than(hi: f64) -> Compiled {
    Compiled::Range { lo: f64::NEG_INFINITY, lo_incl: true, hi, hi_incl: false }
}
/// Both ends included.
fn closed(lo: f64, hi: f64) -> Compiled {
    Compiled::Range { lo, lo_incl: true, hi, hi_incl: true }
}
/// Start included, end not -- how a span of days is meant.
fn half_open(lo: f64, hi: f64) -> Compiled {
    Compiled::Range { lo, lo_incl: true, hi, hi_incl: false }
}
/// Matches nothing (an aggregate over an empty column).
fn nothing() -> Compiled {
    Compiled::Range { lo: 1.0, lo_incl: true, hi: 0.0, hi_incl: true }
}

fn unresolved(c: &Compiled) -> bool {
    matches!(c, Compiled::Aggregate { resolved: None, .. })
}

fn compile_condition(
    kind: ColumnValueKind,
    cond: &Condition,
    now: chrono::DateTime<Local>,
) -> Result<Compiled, String> {
    let need = |text: &str, what: &str| -> Result<String, String> {
        let t = text.trim();
        if t.is_empty() {
            Err(format!("enter {what}"))
        } else {
            Ok(t.to_string())
        }
    };
    let number = |text: &str| -> Result<f64, String> {
        let t = need(text, "a value")?;
        let parsed = match kind {
            ColumnValueKind::Duration => parse_duration_secs(&t),
            ColumnValueKind::DateTime => parse_local_datetime(&t),
            _ => t.parse::<f64>().ok().filter(|v| v.is_finite()),
        };
        parsed.ok_or_else(|| format!("\"{t}\" is not a valid {}", kind_noun(kind)))
    };
    Ok(match cond.op {
        ConditionOp::Text(op) => match op {
            TextOp::Blank => Compiled::Blank,
            TextOp::NotBlank => Compiled::NotBlank,
            TextOp::Equals | TextOp::NotEquals => {
                let t = need(&cond.a, "a value")?;
                let matcher = if TextMatcher::has_wildcards(&t) {
                    TextMatcher::wildcard(&t)
                } else {
                    TextMatcher::wildcard(&t.replace('~', "~~"))
                };
                Compiled::Text {
                    test: TextTest::Equals(matcher),
                    negate: op == TextOp::NotEquals,
                }
            }
            TextOp::Contains | TextOp::NotContains => Compiled::Text {
                test: TextTest::Contains(need(&cond.a, "a value")?.to_lowercase()),
                negate: op == TextOp::NotContains,
            },
            TextOp::BeginsWith => Compiled::Text {
                test: TextTest::BeginsWith(need(&cond.a, "a value")?.to_lowercase()),
                negate: false,
            },
            TextOp::EndsWith => Compiled::Text {
                test: TextTest::EndsWith(need(&cond.a, "a value")?.to_lowercase()),
                negate: false,
            },
            TextOp::MatchesRegex => Compiled::Text {
                test: TextTest::Regex(
                    TextMatcher::regex(&need(&cond.a, "a pattern")?)
                        .map_err(|e| format!("invalid regex: {e}"))?,
                ),
                negate: false,
            },
        },
        ConditionOp::Number(op) => match op {
            NumberOp::Blank => Compiled::Blank,
            NumberOp::NotBlank => Compiled::NotBlank,
            NumberOp::Equals => {
                let v = number(&cond.a)?;
                closed(v - NUMBER_EQ_EPSILON, v + NUMBER_EQ_EPSILON)
            }
            NumberOp::NotEquals => Compiled::NotEqual(number(&cond.a)?),
            NumberOp::Greater => more_than(number(&cond.a)?),
            NumberOp::GreaterOrEqual => at_least(number(&cond.a)?),
            NumberOp::Less => less_than(number(&cond.a)?),
            NumberOp::LessOrEqual => at_most(number(&cond.a)?),
            NumberOp::Between => {
                let (a, b) = (number(&cond.a)?, number(&cond.b)?);
                closed(a.min(b), a.max(b))
            }
            NumberOp::TopItems | NumberOp::BottomItems => {
                let n: f64 = cond
                    .a
                    .trim()
                    .parse()
                    .ok()
                    .filter(|v: &f64| v.is_finite() && *v >= 1.0)
                    .ok_or("enter a count of 1 or more")?;
                Compiled::Aggregate { op, n: n.floor(), resolved: None }
            }
            NumberOp::TopPercent | NumberOp::BottomPercent => {
                let n: f64 = cond
                    .a
                    .trim()
                    .trim_end_matches('%')
                    .trim()
                    .parse()
                    .ok()
                    .filter(|v: &f64| *v > 0.0 && *v <= 100.0)
                    .ok_or("enter a percentage from 1 to 100")?;
                Compiled::Aggregate { op, n, resolved: None }
            }
            NumberOp::AboveAverage | NumberOp::BelowAverage => {
                Compiled::Aggregate { op, n: 0.0, resolved: None }
            }
        },
        ConditionOp::Date(op) => match op {
            DateOp::Blank => Compiled::Blank,
            DateOp::NotBlank => Compiled::NotBlank,
            DateOp::On => {
                let day = day_start(number(&cond.a)?);
                half_open(day, day + SECS_PER_DAY)
            }
            DateOp::Before => less_than(day_start(number(&cond.a)?)),
            // "After 27 Sep" starts once 27 Sep is over, as in Excel.
            DateOp::After => at_least(day_start(number(&cond.a)?) + SECS_PER_DAY),
            DateOp::Between => {
                let (a, b) = (number(&cond.a)?, number(&cond.b)?);
                half_open(day_start(a.min(b)), day_start(a.max(b)) + SECS_PER_DAY)
            }
            DateOp::LastNDays => {
                let n: i64 = cond
                    .a
                    .trim()
                    .parse()
                    .ok()
                    .filter(|v: &i64| *v >= 1)
                    .ok_or("enter a number of days of 1 or more")?;
                let today = local_midnight(now.date_naive());
                half_open(today - (n - 1) as f64 * SECS_PER_DAY, today + SECS_PER_DAY)
            }
            relative => relative_date_range(relative, now),
        },
    })
}

fn kind_noun(kind: ColumnValueKind) -> &'static str {
    match kind {
        ColumnValueKind::Text => "text",
        ColumnValueKind::Number => "number",
        ColumnValueKind::Duration => "time",
        ColumnValueKind::DateTime => "date",
    }
}

const SECS_PER_DAY: f64 = 86_400.0;

fn local_midnight(day: NaiveDate) -> f64 {
    Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap_or_default())
        .earliest()
        .map(|t| t.timestamp() as f64)
        .unwrap_or(0.0)
}

fn day_start(secs: f64) -> f64 {
    Local
        .timestamp_opt(secs.floor() as i64, 0)
        .single()
        .map(|t| local_midnight(t.date_naive()))
        .unwrap_or(secs)
}

fn relative_date_range(op: DateOp, now: chrono::DateTime<Local>) -> Compiled {
    let today = now.date_naive();
    let from = |d: NaiveDate| local_midnight(d);
    // Weeks start on Monday (ISO); Excel follows the locale, and a Japanese
    // locale starts them on Sunday -- a detail not worth a setting yet.
    let week_start = today - ChronoDuration::days(today.weekday().num_days_from_monday() as i64);
    let month_start = today.with_day(1).unwrap_or(today);
    let (lo, hi) = match op {
        DateOp::Today => (from(today), from(today + ChronoDuration::days(1))),
        DateOp::Yesterday => (from(today - ChronoDuration::days(1)), from(today)),
        DateOp::ThisWeek => (from(week_start), from(week_start + ChronoDuration::days(7))),
        DateOp::LastWeek => (from(week_start - ChronoDuration::days(7)), from(week_start)),
        DateOp::ThisMonth => {
            let next = month_start
                .checked_add_months(chrono::Months::new(1))
                .unwrap_or(month_start);
            (from(month_start), from(next))
        }
        DateOp::LastMonth => {
            let prev = month_start
                .checked_sub_months(chrono::Months::new(1))
                .unwrap_or(month_start);
            (from(prev), from(month_start))
        }
        DateOp::ThisYear => {
            let jan1 = NaiveDate::from_ymd_opt(today.year(), 1, 1).unwrap_or(today);
            let next = NaiveDate::from_ymd_opt(today.year() + 1, 1, 1).unwrap_or(today);
            (from(jan1), from(next))
        }
        _ => (f64::NEG_INFINITY, f64::INFINITY),
    };
    half_open(lo, hi)
}

fn resolve(c: &mut Compiled, values: &[f64]) {
    let Compiled::Aggregate { op, n, resolved } = c else {
        return;
    };
    let mut finite: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    let range = if finite.is_empty() {
        // Nothing to rank or average: nothing passes, like an empty column.
        nothing()
    } else {
        match op {
            NumberOp::AboveAverage | NumberOp::BelowAverage => {
                let mean = finite.iter().sum::<f64>() / finite.len() as f64;
                if *op == NumberOp::AboveAverage {
                    more_than(mean)
                } else {
                    less_than(mean)
                }
            }
            _ => {
                let count = match op {
                    NumberOp::TopPercent | NumberOp::BottomPercent => {
                        ((finite.len() as f64) * *n / 100.0).ceil() as usize
                    }
                    _ => *n as usize,
                }
                .clamp(1, finite.len());
                let top = matches!(op, NumberOp::TopItems | NumberOp::TopPercent);
                if top {
                    finite.sort_by(|a, b| b.total_cmp(a));
                } else {
                    finite.sort_by(|a, b| a.total_cmp(b));
                }
                // Rows tied with the Nth value are all kept, as in Excel.
                let edge = finite[count - 1];
                if top {
                    at_least(edge)
                } else {
                    at_most(edge)
                }
            }
        }
    };
    *resolved = Some(Box::new(range));
}

fn eval(c: &Compiled, value: &CellValue) -> bool {
    match c {
        Compiled::Blank => value.is_blank(),
        Compiled::NotBlank => !value.is_blank(),
        Compiled::Text { test, negate } => {
            let texts: Vec<&str> = match value {
                CellValue::Text(t) => vec![t.as_ref()],
                CellValue::Texts(v) => v.iter().map(|t| t.as_ref()).collect(),
                CellValue::Number(_) | CellValue::Missing => Vec::new(),
            };
            let hit = texts.iter().any(|t| text_test(test, t));
            if *negate {
                !hit
            } else {
                hit
            }
        }
        Compiled::Range { lo, lo_incl, hi, hi_incl } => match value.number() {
            Some(v) => {
                let above = if *lo_incl { v >= *lo } else { v > *lo };
                let below = if *hi_incl { v <= *hi } else { v < *hi };
                above && below
            }
            None => false,
        },
        Compiled::NotEqual(x) => match value.number() {
            Some(v) => (v - x).abs() > NUMBER_EQ_EPSILON,
            // Excel keeps blanks for "does not equal".
            None => true,
        },
        Compiled::Aggregate { resolved, .. } => match resolved {
            Some(range) => eval(range, value),
            None => true,
        },
    }
}

fn text_test(test: &TextTest, text: &str) -> bool {
    match test {
        TextTest::Equals(m) | TextTest::Regex(m) => m.is_match(text),
        TextTest::Contains(needle) => contains_ignore_case(text, needle),
        TextTest::BeginsWith(prefix) => lowercase_if_needed(text).starts_with(prefix.as_str()),
        TextTest::EndsWith(suffix) => lowercase_if_needed(text).ends_with(suffix.as_str()),
    }
}

fn lowercase_if_needed(text: &str) -> Cow<'_, str> {
    if text.chars().any(char::is_uppercase) {
        Cow::Owned(text.to_lowercase())
    } else {
        Cow::Borrowed(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> chrono::DateTime<Local> {
        // A Wednesday.
        Local.with_ymd_and_hms(2026, 9, 23, 15, 0, 0).unwrap()
    }

    fn cond(op: ConditionOp, a: &str) -> Condition {
        Condition { op, a: a.into(), b: String::new() }
    }

    fn one(kind: ColumnValueKind, c: Condition) -> CompiledColumnFilter {
        CompiledColumnFilter::compile(kind, &FilterRule::Conditions { first: c, second: None }, now())
            .unwrap()
    }

    fn t(s: &str) -> CellValue<'_> {
        CellValue::Text(Cow::Borrowed(s))
    }

    #[test]
    fn text_conditions_are_case_insensitive() {
        use TextOp::*;
        let f = |op, a| one(ColumnValueKind::Text, cond(ConditionOp::Text(op), a));
        assert!(f(Contains, "KICK").matches(&t("big_kick.wav")));
        assert!(!f(NotContains, "kick").matches(&t("big_KICK.wav")));
        assert!(f(BeginsWith, "Big").matches(&t("big_kick.wav")));
        assert!(f(EndsWith, ".WAV").matches(&t("big_kick.wav")));
        assert!(f(Equals, "BIG_KICK.wav").matches(&t("big_kick.wav")));
        assert!(!f(Equals, "big").matches(&t("big_kick.wav")));
        assert!(f(NotEquals, "big").matches(&t("big_kick.wav")));
    }

    #[test]
    fn equals_takes_excel_wildcards() {
        let f = one(ColumnValueKind::Text, cond(ConditionOp::Text(TextOp::Equals), "kick_??.wav"));
        assert!(f.matches(&t("Kick_01.wav")));
        assert!(!f.matches(&t("kick_1.wav")));
    }

    #[test]
    fn regex_condition_reports_a_bad_pattern() {
        let err = CompiledColumnFilter::compile(
            ColumnValueKind::Text,
            &FilterRule::Conditions {
                first: cond(ConditionOp::Text(TextOp::MatchesRegex), "(ab"),
                second: None,
            },
            now(),
        )
        .unwrap_err();
        assert_eq!(err.condition, 0);
        assert!(err.message.contains("regex"));
    }

    #[test]
    fn tags_match_if_any_tag_does() {
        let tags = CellValue::Texts(vec!["drums".into(), "final".into()]);
        let has_final = one(ColumnValueKind::Text, cond(ConditionOp::Text(TextOp::Equals), "final"));
        assert!(has_final.matches(&tags));
        let no_draft =
            one(ColumnValueKind::Text, cond(ConditionOp::Text(TextOp::NotContains), "draft"));
        assert!(no_draft.matches(&tags));
        let no_drums =
            one(ColumnValueKind::Text, cond(ConditionOp::Text(TextOp::NotContains), "drum"));
        assert!(!no_drums.matches(&tags));
    }

    #[test]
    fn number_comparisons_and_between() {
        use NumberOp::*;
        let f = |op, a: &str, b: &str| {
            CompiledColumnFilter::compile(
                ColumnValueKind::Number,
                &FilterRule::Conditions {
                    first: Condition { op: ConditionOp::Number(op), a: a.into(), b: b.into() },
                    second: None,
                },
                now(),
            )
            .unwrap()
        };
        let v = CellValue::Number(-14.0);
        assert!(f(Equals, "-14", "").matches(&v));
        assert!(f(NotEquals, "-13", "").matches(&v));
        assert!(f(Greater, "-15", "").matches(&v));
        assert!(!f(Greater, "-14", "").matches(&v));
        assert!(f(GreaterOrEqual, "-14", "").matches(&v));
        assert!(!f(Less, "-14", "").matches(&v));
        assert!(f(LessOrEqual, "-14", "").matches(&v));
        assert!(f(Between, "-10", "-20").matches(&v), "bounds in either order");
        assert!(!f(Greater, "0", "").matches(&CellValue::Missing), "blank fails a comparison");
        assert!(f(NotEquals, "0", "").matches(&CellValue::Missing), "but passes does-not-equal");
    }

    #[test]
    fn and_or_join_two_conditions() {
        let rule = |join| FilterRule::Conditions {
            first: cond(ConditionOp::Number(NumberOp::Less), "-20"),
            second: Some((join, cond(ConditionOp::Number(NumberOp::Greater), "-10"))),
        };
        let or = CompiledColumnFilter::compile(ColumnValueKind::Number, &rule(Join::Or), now()).unwrap();
        assert!(or.matches(&CellValue::Number(-25.0)));
        assert!(or.matches(&CellValue::Number(-5.0)));
        assert!(!or.matches(&CellValue::Number(-15.0)));
        let and = CompiledColumnFilter::compile(ColumnValueKind::Number, &rule(Join::And), now()).unwrap();
        assert!(!and.matches(&CellValue::Number(-25.0)));
    }

    #[test]
    fn top_items_keep_ties_and_percent_rounds_up() {
        let values = [5.0, 9.0, 9.0, 1.0, 7.0];
        let mut top2 = one(ColumnValueKind::Number, cond(ConditionOp::Number(NumberOp::TopItems), "2"));
        assert!(top2.needs_aggregate());
        top2.resolve_aggregates(&values);
        assert!(!top2.needs_aggregate());
        let kept: Vec<f64> = values.iter().copied().filter(|v| top2.matches(&CellValue::Number(*v))).collect();
        assert_eq!(kept, vec![9.0, 9.0]);

        // 20% of 5 values is exactly 1; 30% rounds up to 2.
        let mut bottom =
            one(ColumnValueKind::Number, cond(ConditionOp::Number(NumberOp::BottomPercent), "30"));
        bottom.resolve_aggregates(&values);
        let kept: Vec<f64> = values.iter().copied().filter(|v| bottom.matches(&CellValue::Number(*v))).collect();
        assert_eq!(kept, vec![5.0, 1.0]);
    }

    #[test]
    fn average_splits_the_column() {
        let values = [1.0, 2.0, 3.0, 10.0];
        let mut above =
            one(ColumnValueKind::Number, cond(ConditionOp::Number(NumberOp::AboveAverage), ""));
        above.resolve_aggregates(&values);
        assert!(above.matches(&CellValue::Number(10.0)));
        assert!(!above.matches(&CellValue::Number(4.0)), "mean is 4, strictly above");
        let mut below =
            one(ColumnValueKind::Number, cond(ConditionOp::Number(NumberOp::BelowAverage), ""));
        below.resolve_aggregates(&values);
        assert!(below.matches(&CellValue::Number(3.0)));
    }

    #[test]
    fn durations_take_clock_notation() {
        assert_eq!(parse_duration_secs("83.5"), Some(83.5));
        assert_eq!(parse_duration_secs("83.5s"), Some(83.5));
        assert_eq!(parse_duration_secs("1:23.5"), Some(83.5));
        assert_eq!(parse_duration_secs("1:02:03"), Some(3723.0));
        assert_eq!(parse_duration_secs("abc"), None);
        assert_eq!(parse_duration_secs("-3"), None);
        let longer = one(
            ColumnValueKind::Duration,
            cond(ConditionOp::Number(NumberOp::Greater), "1:00"),
        );
        assert!(longer.matches(&CellValue::Number(61.0)));
        assert!(!longer.matches(&CellValue::Number(59.0)));
    }

    #[test]
    fn a_bad_number_names_the_kind() {
        let err = CompiledColumnFilter::compile(
            ColumnValueKind::Duration,
            &FilterRule::Conditions {
                first: cond(ConditionOp::Number(NumberOp::Greater), "soon"),
                second: None,
            },
            now(),
        )
        .unwrap_err();
        assert!(err.message.contains("time"), "{}", err.message);
    }

    fn at(y: i32, m: u32, d: u32, h: u32) -> CellValue<'static> {
        CellValue::Number(Local.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap().timestamp() as f64)
    }

    #[test]
    fn dates_compare_by_day() {
        use DateOp::*;
        let f = |op, a: &str, b: &str| {
            CompiledColumnFilter::compile(
                ColumnValueKind::DateTime,
                &FilterRule::Conditions {
                    first: Condition { op: ConditionOp::Date(op), a: a.into(), b: b.into() },
                    second: None,
                },
                now(),
            )
            .unwrap()
        };
        assert!(f(On, "2026-09-20", "").matches(&at(2026, 9, 20, 23)));
        assert!(!f(On, "2026-09-20", "").matches(&at(2026, 9, 21, 0)));
        assert!(f(Before, "2026/09/20", "").matches(&at(2026, 9, 19, 23)));
        assert!(!f(Before, "2026-09-20", "").matches(&at(2026, 9, 20, 1)));
        assert!(!f(After, "2026-09-20", "").matches(&at(2026, 9, 20, 23)));
        assert!(f(After, "2026-09-20", "").matches(&at(2026, 9, 21, 0)));
        assert!(f(Between, "2026-09-22", "2026-09-20").matches(&at(2026, 9, 22, 23)));
    }

    #[test]
    fn relative_dates_follow_the_calendar() {
        use DateOp::*;
        let f = |op, a: &str| one(ColumnValueKind::DateTime, cond(ConditionOp::Date(op), a));
        assert!(f(Today, "").matches(&at(2026, 9, 23, 1)));
        assert!(f(Yesterday, "").matches(&at(2026, 9, 22, 12)));
        // Week of Mon 21 Sep.
        assert!(f(ThisWeek, "").matches(&at(2026, 9, 21, 0)));
        assert!(!f(ThisWeek, "").matches(&at(2026, 9, 20, 23)));
        assert!(f(LastWeek, "").matches(&at(2026, 9, 14, 9)));
        assert!(f(ThisMonth, "").matches(&at(2026, 9, 1, 0)));
        assert!(f(LastMonth, "").matches(&at(2026, 8, 31, 23)));
        assert!(!f(LastMonth, "").matches(&at(2026, 9, 1, 0)));
        assert!(f(ThisYear, "").matches(&at(2026, 1, 1, 0)));
        assert!(f(LastNDays, "3").matches(&at(2026, 9, 21, 0)));
        assert!(!f(LastNDays, "3").matches(&at(2026, 9, 20, 23)));
    }

    #[test]
    fn value_list_matches_the_text_it_shows() {
        let mut sel = ValueSelection::default();
        sel.values.insert("48000".into());
        sel.values.insert("0:03.0".into());
        let sr = CompiledColumnFilter::compile(ColumnValueKind::Number, &FilterRule::Values(sel.clone()), now()).unwrap();
        assert!(sr.matches(&CellValue::Number(48_000.0)));
        assert!(!sr.matches(&CellValue::Number(44_100.0)));
        assert!(!sr.matches(&CellValue::Missing));
        sel.blank = true;
        let with_blank =
            CompiledColumnFilter::compile(ColumnValueKind::Number, &FilterRule::Values(sel), now()).unwrap();
        assert!(with_blank.matches(&CellValue::Missing));
        let shown = display_text(ColumnValueKind::Duration, &CellValue::Number(3.0));
        assert_eq!(shown.len(), 1);
    }

    #[test]
    fn rules_roundtrip_through_json() {
        let rule = FilterRule::Conditions {
            first: cond(ConditionOp::Number(NumberOp::TopPercent), "10"),
            second: Some((Join::Or, cond(ConditionOp::Number(NumberOp::Blank), ""))),
        };
        let json = serde_json::to_string(&rule).unwrap();
        let back: FilterRule = serde_json::from_str(&json).unwrap();
        assert_eq!(rule, back);
    }
}
