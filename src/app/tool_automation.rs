//! Automation of the EQ, Compressor and Limiter as rules, with no UI: which
//! of a tool's settings can follow a curve (`AutoParam`), what each is
//! called and how it is drawn (unit, range, scale), the curves a tab holds
//! (`AutomationLane`), and the `Tracks` its render runs with. The DSP side
//! (evaluating a curve, the time-varying processors) is `crate::automation`,
//! `crate::eq` and `crate::dynamics`; the canvas and the Inspector draw and
//! edit the lanes through this file.
//!
//! A lane's points are `(sample, value)` in the tab's buffer samples and the
//! parameter's own units, like a gain curve's. Lanes are tab state edited as
//! part of the tool's draft (undoable step by step), and are not saved in a
//! session -- the gain and pitch curves are not either.

use std::ops::RangeInclusive;

use super::tool_render::TimeMap;
use super::types::{EditorTab, ToolKind, ToolState};
use crate::automation::{Automatable, Curve, Scale, Tracks};
use crate::dynamics::{CompParam, LimiterParam};
use crate::eq::{EqKind, EqParam, EQ_MAX_BANDS};

/// A setting of the EQ, Compressor or Limiter that can follow a curve.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AutoParam {
    Eq(EqParam),
    Comp(CompParam),
    Limiter(LimiterParam),
}

/// One automated setting on a tab: its curve, and whether the curve is in
/// use (`false` keeps the points but renders the static setting).
#[derive(Clone, Debug, PartialEq)]
pub struct AutomationLane {
    pub param: AutoParam,
    pub enabled: bool,
    pub points: Vec<(usize, f32)>,
}

impl AutomationLane {
    /// In use and with something to follow.
    pub fn is_active(&self) -> bool {
        self.enabled && !self.points.is_empty()
    }
}

impl AutoParam {
    /// The tool this setting belongs to.
    pub fn tool(self) -> ToolKind {
        match self {
            AutoParam::Eq(_) => ToolKind::Eq,
            AutoParam::Comp(_) => ToolKind::Compressor,
            AutoParam::Limiter(_) => ToolKind::Limiter,
        }
    }

    /// The settings a tool's Automation section offers, in its order. The
    /// EQ offers the band picked in its panel, then the output.
    pub fn for_tool(tool: ToolKind, eq_band: usize, st: &ToolState) -> Vec<AutoParam> {
        match tool {
            ToolKind::Compressor => CompParam::ALL.into_iter().map(AutoParam::Comp).collect(),
            ToolKind::Limiter => LimiterParam::ALL
                .into_iter()
                .map(AutoParam::Limiter)
                .collect(),
            ToolKind::Eq => {
                let mut out = Vec::new();
                if let Some(band) = st.eq.bands.get(eq_band).filter(|b| b.enabled) {
                    let b = eq_band as u8;
                    out.push(AutoParam::Eq(EqParam::Freq(b)));
                    if band.kind.uses_gain() {
                        out.push(AutoParam::Eq(EqParam::Gain(b)));
                    }
                    out.push(AutoParam::Eq(EqParam::Q(b)));
                }
                out.push(AutoParam::Eq(EqParam::Output));
                out
            }
            _ => Vec::new(),
        }
    }

    /// The name in a list of every lane ("Band 2 Freq", "Threshold").
    pub fn label(self) -> String {
        match self {
            AutoParam::Eq(EqParam::Output) => "Output".into(),
            AutoParam::Eq(p) => {
                format!("Band {} {}", p.band().unwrap_or(0) + 1, self.short_label())
            }
            _ => self.short_label().into(),
        }
    }

    /// The name within its own band or tool ("Freq", "Threshold").
    pub fn short_label(self) -> &'static str {
        match self {
            AutoParam::Eq(EqParam::Freq(_)) => "Freq",
            AutoParam::Eq(EqParam::Gain(_)) => "Gain",
            AutoParam::Eq(EqParam::Q(_)) => "Q",
            AutoParam::Eq(EqParam::Output) => "Output",
            AutoParam::Comp(p) => match p {
                CompParam::Threshold => "Threshold",
                CompParam::Ratio => "Ratio",
                CompParam::Knee => "Knee",
                CompParam::Attack => "Attack",
                CompParam::Release => "Release",
                CompParam::Makeup => "Makeup",
                CompParam::Mix => "Mix",
                CompParam::Range => "Range",
                CompParam::Link => "Link",
                CompParam::SidechainHpf => "Sidechain HPF",
                CompParam::RmsWindow => "RMS window",
            },
            AutoParam::Limiter(p) => match p {
                LimiterParam::InputGain => "Input",
                LimiterParam::Ceiling => "Ceiling",
                LimiterParam::Release => "Release",
            },
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            AutoParam::Eq(EqParam::Freq(_)) | AutoParam::Comp(CompParam::SidechainHpf) => " Hz",
            AutoParam::Eq(EqParam::Q(_)) => "",
            AutoParam::Comp(CompParam::Ratio) => ":1",
            AutoParam::Comp(CompParam::Mix | CompParam::Link) => " %",
            AutoParam::Comp(CompParam::Attack | CompParam::Release | CompParam::RmsWindow)
            | AutoParam::Limiter(LimiterParam::Release) => " ms",
            _ => " dB",
        }
    }

    pub fn decimals(self) -> usize {
        match self {
            AutoParam::Eq(EqParam::Freq(_))
            | AutoParam::Comp(CompParam::SidechainHpf | CompParam::Mix | CompParam::Link)
            | AutoParam::Limiter(LimiterParam::Release) => 0,
            AutoParam::Eq(EqParam::Q(_)) => 2,
            _ => 1,
        }
    }

    pub fn scale(self) -> Scale {
        match self {
            AutoParam::Eq(p) => p.scale(),
            AutoParam::Comp(p) => p.scale(),
            AutoParam::Limiter(p) => p.scale(),
        }
    }

    /// The values the setting takes. An EQ band's Q depends on its shape.
    pub fn range(self, st: &ToolState) -> RangeInclusive<f32> {
        match self {
            AutoParam::Eq(p) => {
                let kind = p
                    .band()
                    .and_then(|b| st.eq.bands.get(b))
                    .map_or(EqKind::Bell, |b| b.kind);
                p.range(kind)
            }
            AutoParam::Comp(p) => p.range(),
            AutoParam::Limiter(p) => p.range(),
        }
    }

    /// The static setting: what the curve replaces.
    pub fn static_value(self, st: &ToolState) -> f32 {
        match self {
            AutoParam::Eq(p) => st.eq.param(p),
            AutoParam::Comp(p) => st.compressor.param(p),
            AutoParam::Limiter(p) => st.limiter.param(p),
        }
    }

    /// Whether the setting changes anything with the tool as it is set now:
    /// a cut has no gain, auto makeup ignores Makeup, Peak detection has no
    /// RMS window. Such a lane is kept but drawn as idle.
    pub fn has_effect(self, st: &ToolState) -> bool {
        match self {
            AutoParam::Eq(p) => p.band().map_or(true, |b| {
                st.eq.bands.get(b).is_some_and(|band| {
                    band.is_heard() && (!matches!(p, EqParam::Gain(_)) || band.kind.uses_gain())
                })
            }),
            AutoParam::Comp(CompParam::Makeup) => !st.compressor.auto_makeup,
            AutoParam::Comp(CompParam::RmsWindow) => {
                st.compressor.detection == crate::dynamics::Detection::Rms
            }
            _ => true,
        }
    }

    /// `value` as a height in `[0, 1]` over the setting's range, on its
    /// scale: where the canvas draws it.
    pub fn to_frac(self, st: &ToolState, value: f32) -> f32 {
        let r = self.range(st);
        let (lo, hi) = (*r.start(), *r.end());
        let t = match self.scale() {
            Scale::Log if lo > 0.0 => (value.max(lo) / lo).ln() / (hi / lo).ln(),
            _ => (value - lo) / (hi - lo).max(1e-9),
        };
        t.clamp(0.0, 1.0)
    }

    /// The value at height `frac`.
    pub fn from_frac(self, st: &ToolState, frac: f32) -> f32 {
        let r = self.range(st);
        let (lo, hi) = (*r.start(), *r.end());
        let t = frac.clamp(0.0, 1.0);
        match self.scale() {
            Scale::Log if lo > 0.0 => lo * (hi / lo).powf(t),
            _ => lo + (hi - lo) * t,
        }
    }

    /// `value` with its unit, at the setting's precision.
    pub fn format(self, value: f32) -> String {
        format!("{:.*}{}", self.decimals(), value, self.unit())
    }
}

impl super::WavesPreviewer {
    /// Where the Inspector shows automated settings: the playhead, in the
    /// tab's buffer samples, when the engine is on this tab; else the start.
    pub(in crate::app) fn automation_playhead(&self, tab_idx: usize) -> usize {
        if self.tool_preview_tab_is_audible(tab_idx) {
            self.tool_preview_source_pos(tab_idx).max(0.0) as usize
        } else {
            0
        }
    }
}

// ---------------------------------------------------------------------------
// The lanes on a tab.

/// The lane of `param`, if the tab has one.
pub fn lane(tab: &EditorTab, param: AutoParam) -> Option<&AutomationLane> {
    tab.automation.iter().find(|l| l.param == param)
}

/// Whether `param` follows a curve now.
pub fn is_automated(tab: &EditorTab, param: AutoParam) -> bool {
    lane(tab, param).is_some_and(AutomationLane::is_active)
}

/// The lane of `param`, made (on, with no points) if there is none.
pub fn lane_mut(tab: &mut EditorTab, param: AutoParam) -> &mut AutomationLane {
    if let Some(idx) = tab.automation.iter().position(|l| l.param == param) {
        return &mut tab.automation[idx];
    }
    tab.automation.push(AutomationLane {
        param,
        enabled: true,
        points: Vec::new(),
    });
    tab.automation.last_mut().expect("just pushed")
}

/// Forget every lane of `tool` (its Reset).
pub fn clear_tool(tab: &mut EditorTab, tool: ToolKind) {
    tab.automation.retain(|l| l.param.tool() != tool);
    if tab.automation_selected.is_some_and(|p| p.tool() == tool) {
        tab.automation_drag = None;
    }
}

/// Forget the lanes of EQ band `band` (the band was deleted).
pub fn clear_eq_band(tab: &mut EditorTab, band: usize) {
    tab.automation
        .retain(|l| !matches!(l.param, AutoParam::Eq(p) if p.band() == Some(band)));
}

/// The lanes of every EQ band slot no longer in use.
pub fn prune_unused_eq_bands(tab: &mut EditorTab) {
    for band in 0..EQ_MAX_BANDS {
        if !tab.tool_state.eq.bands[band].enabled {
            clear_eq_band(tab, band);
        }
    }
}

/// Every point through `map`, for an edit that moved audio; points past
/// `len` are dropped.
pub fn remap(tab: &mut EditorTab, map: &TimeMap, len: usize) {
    if matches!(map, TimeMap::Identity) {
        return;
    }
    for lane in &mut tab.automation {
        for point in &mut lane.points {
            point.0 = map.map_sample(point.0);
        }
        lane.points.retain(|(s, _)| *s <= len);
        lane.points.sort_by_key(|(s, _)| *s);
    }
}

fn tracks<P: Copy + PartialEq>(
    tab: &EditorTab,
    pick: impl Fn(AutoParam) -> Option<P>,
) -> Tracks<P> {
    let mut out = Tracks::none();
    for lane in tab.automation.iter().filter(|l| l.is_active()) {
        if let Some(p) = pick(lane.param) {
            out.push(p, Curve::new(lane.points.clone(), lane.param.scale()));
        }
    }
    out
}

/// The EQ's curves as its render runs them.
pub fn eq_tracks(tab: &EditorTab) -> Tracks<EqParam> {
    tracks(tab, |p| match p {
        AutoParam::Eq(p) => Some(p),
        _ => None,
    })
}

pub fn comp_tracks(tab: &EditorTab) -> Tracks<CompParam> {
    tracks(tab, |p| match p {
        AutoParam::Comp(p) => Some(p),
        _ => None,
    })
}

pub fn limiter_tracks(tab: &EditorTab) -> Tracks<LimiterParam> {
    tracks(tab, |p| match p {
        AutoParam::Limiter(p) => Some(p),
        _ => None,
    })
}

/// The tool's settings at `sample`, curves applied: what the Inspector
/// shows while the playhead is there.
pub fn tool_state_at(tab: &EditorTab, sample: usize) -> ToolState {
    let mut st = tab.tool_state;
    if tab.automation.iter().any(AutomationLane::is_active) {
        st.eq = crate::automation::settings_at(&st.eq, &eq_tracks(tab), sample);
        st.compressor = crate::automation::settings_at(&st.compressor, &comp_tracks(tab), sample);
        st.limiter = crate::automation::settings_at(&st.limiter, &limiter_tracks(tab), sample);
    }
    st
}

/// Take back what the Inspector's graphs changed in `shown` (settings at
/// the playhead) into `tab`'s static settings -- except automated fields,
/// whose value on screen is the curve's, not something to keep.
pub fn merge_unautomated(tab: &mut EditorTab, before: &ToolState, shown: &ToolState) {
    let automated: Vec<AutoParam> = tab
        .automation
        .iter()
        .filter(|l| l.is_active())
        .map(|l| l.param)
        .collect();
    let keep = |p: AutoParam| !automated.contains(&p);
    // EQ: every band field, field by field; the switches are never automated.
    for b in 0..EQ_MAX_BANDS {
        let (was, now) = (before.eq.bands[b], shown.eq.bands[b]);
        if was == now {
            continue;
        }
        let dst = &mut tab.tool_state.eq.bands[b];
        let slot = b as u8;
        if was.freq_hz != now.freq_hz && keep(AutoParam::Eq(EqParam::Freq(slot))) {
            dst.freq_hz = now.freq_hz;
        }
        if was.gain_db != now.gain_db && keep(AutoParam::Eq(EqParam::Gain(slot))) {
            dst.gain_db = now.gain_db;
        }
        if keep(AutoParam::Eq(EqParam::Q(slot))) {
            if was.q != now.q {
                dst.q = now.q;
            }
            if was.cut_q != now.cut_q {
                dst.cut_q = now.cut_q;
            }
        }
        if was.enabled != now.enabled || was.kind != now.kind {
            // Added, deleted or reshaped: the band is the one on screen.
            *dst = now;
        }
        dst.bypassed = now.bypassed;
        dst.slope = now.slope;
        dst.mode = now.mode;
    }
    if before.eq.output_db != shown.eq.output_db && keep(AutoParam::Eq(EqParam::Output)) {
        tab.tool_state.eq.output_db = shown.eq.output_db;
    }
    for p in CompParam::ALL {
        let (was, now) = (before.compressor.param(p), shown.compressor.param(p));
        if was != now && keep(AutoParam::Comp(p)) {
            tab.tool_state.compressor.set_param(p, now);
        }
    }
    for p in LimiterParam::ALL {
        let (was, now) = (before.limiter.param(p), shown.limiter.param(p));
        if was != now && keep(AutoParam::Limiter(p)) {
            tab.tool_state.limiter.set_param(p, now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab() -> EditorTab {
        EditorTab::new_base(std::path::PathBuf::from("/a.wav"), "a.wav".into())
    }

    #[test]
    fn every_tool_offers_its_settings() {
        let st = ToolState::default_values();
        assert_eq!(AutoParam::for_tool(ToolKind::Compressor, 0, &st).len(), 11);
        assert_eq!(AutoParam::for_tool(ToolKind::Limiter, 0, &st).len(), 3);
        // The default EQ's band 2 is a bell: freq, gain, Q, then the output.
        let eq = AutoParam::for_tool(ToolKind::Eq, 1, &st);
        assert_eq!(
            eq,
            vec![
                AutoParam::Eq(EqParam::Freq(1)),
                AutoParam::Eq(EqParam::Gain(1)),
                AutoParam::Eq(EqParam::Q(1)),
                AutoParam::Eq(EqParam::Output),
            ]
        );
        assert!(AutoParam::for_tool(ToolKind::Gain, 0, &st).is_empty());
        assert_eq!(AutoParam::Eq(EqParam::Freq(1)).label(), "Band 2 Freq");
    }

    #[test]
    fn heights_round_trip_on_each_scale() {
        let st = ToolState::default_values();
        for p in [
            AutoParam::Eq(EqParam::Freq(1)),
            AutoParam::Comp(CompParam::Threshold),
            AutoParam::Comp(CompParam::Ratio),
            AutoParam::Limiter(LimiterParam::Release),
        ] {
            for frac in [0.0f32, 0.25, 0.5, 0.9, 1.0] {
                let v = p.from_frac(&st, frac);
                assert!((p.to_frac(&st, v) - frac).abs() < 1e-4, "{p:?} {frac}");
            }
        }
        // A log axis puts 1 kHz near the middle of 20 Hz .. 20 kHz.
        let mid = AutoParam::Eq(EqParam::Freq(0)).from_frac(&st, 0.5);
        assert!((mid - 632.5).abs() < 5.0, "{mid}");
    }

    #[test]
    fn only_active_lanes_become_tracks() {
        let mut t = tab();
        lane_mut(&mut t, AutoParam::Comp(CompParam::Threshold)).points = vec![(0, -20.0)];
        lane_mut(&mut t, AutoParam::Comp(CompParam::Ratio)); // no points
        let off = lane_mut(&mut t, AutoParam::Limiter(LimiterParam::Ceiling));
        off.points = vec![(0, -3.0)];
        off.enabled = false;
        let comp = comp_tracks(&t);
        assert_eq!(
            comp.params().collect::<Vec<_>>(),
            vec![CompParam::Threshold]
        );
        assert!(limiter_tracks(&t).is_empty());
        assert!(is_automated(&t, AutoParam::Comp(CompParam::Threshold)));
        assert!(!is_automated(&t, AutoParam::Comp(CompParam::Ratio)));
        let at = tool_state_at(&t, 500);
        assert_eq!(at.compressor.threshold_db, -20.0);
        assert_eq!(at.limiter.ceiling_db, t.tool_state.limiter.ceiling_db);
    }

    #[test]
    fn a_graph_edit_keeps_only_what_is_not_automated() {
        let mut t = tab();
        lane_mut(&mut t, AutoParam::Comp(CompParam::Threshold)).points = vec![(0, -40.0)];
        let before = tool_state_at(&t, 0);
        let mut shown = before.clone();
        shown.compressor.threshold_db = -10.0; // dragged, but automated
        shown.compressor.ratio = 6.0; // dragged, free
        let static_threshold = t.tool_state.compressor.threshold_db;
        merge_unautomated(&mut t, &before, &shown);
        assert_eq!(t.tool_state.compressor.threshold_db, static_threshold);
        assert_eq!(t.tool_state.compressor.ratio, 6.0);
    }

    #[test]
    fn deleting_a_band_takes_its_lanes_with_it() {
        let mut t = tab();
        lane_mut(&mut t, AutoParam::Eq(EqParam::Freq(1))).points = vec![(0, 300.0)];
        lane_mut(&mut t, AutoParam::Eq(EqParam::Output)).points = vec![(0, -3.0)];
        t.tool_state.eq.bands[1].enabled = false;
        prune_unused_eq_bands(&mut t);
        assert!(lane(&t, AutoParam::Eq(EqParam::Freq(1))).is_none());
        assert!(lane(&t, AutoParam::Eq(EqParam::Output)).is_some());
        clear_tool(&mut t, ToolKind::Eq);
        assert!(t.automation.is_empty());
    }

    #[test]
    fn points_follow_an_edit_that_moves_audio() {
        let mut t = tab();
        lane_mut(&mut t, AutoParam::Limiter(LimiterParam::InputGain)).points =
            vec![(100, 3.0), (500, 6.0), (2_000, 9.0)];
        let map = TimeMap::Range {
            start: 200,
            old_end: 200,
            new_end: 400,
        };
        remap(&mut t, &map, 1_000);
        assert_eq!(
            lane(&t, AutoParam::Limiter(LimiterParam::InputGain))
                .unwrap()
                .points,
            vec![(100, 3.0), (700, 6.0)]
        );
    }
}
