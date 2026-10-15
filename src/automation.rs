//! Parameter automation as data and evaluation, DSP side only: a breakpoint
//! curve over buffer samples per automated parameter, and the rule that
//! turns a tool's static settings plus its curves into the settings in force
//! at any sample. `eq` and `dynamics` run it block by block; the editor
//! draws and edits the same curves (`app::tool_automation`).
//!
//! A curve holds its first value before its first point and its last after
//! its last, like a gain curve: points describe a shape, and nothing outside
//! them jumps back to the static setting.

use std::fmt::Debug;

/// How a parameter moves between two points: in its own units (dB, %), or
/// in their logarithm (Hz, ms, ratios, Q), so that the line drawn straight
/// on a log axis is the sweep that is heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scale {
    Linear,
    Log,
}

/// Automated settings are evaluated afresh this often, in samples (0.7 ms
/// at 48 kHz): fine enough that a gain stepping once a block never zips,
/// coarse enough that redesigning a filter every block costs a fraction of
/// running it.
pub const AUTOMATION_BLOCK: usize = 32;

/// The smallest value a log-scaled curve is interpolated from: a log of
/// zero has no meaning, and every log-scaled parameter is positive anyway.
const LOG_FLOOR: f32 = 1e-6;

/// One parameter's breakpoints, `(sample, value)` in buffer samples and the
/// parameter's own units, sorted by sample.
#[derive(Clone, Debug, PartialEq)]
pub struct Curve {
    pub points: Vec<(usize, f32)>,
    pub scale: Scale,
}

impl Curve {
    /// A curve through `points` (sorted here; order of equal samples kept).
    pub fn new(mut points: Vec<(usize, f32)>, scale: Scale) -> Self {
        points.sort_by_key(|(s, _)| *s);
        Self { points, scale }
    }

    /// The value at `sample`, or `None` for a curve with no points.
    pub fn value_at(&self, sample: usize) -> Option<f32> {
        let upper = self.points.partition_point(|(s, _)| *s <= sample);
        self.value_with_upper(upper, sample)
    }

    /// The smallest value the curve takes.
    pub fn min_value(&self) -> Option<f32> {
        self.points.iter().map(|(_, v)| *v).reduce(f32::min)
    }

    /// The value at `sample`, given `upper`: the index of the first point
    /// after it.
    fn value_with_upper(&self, upper: usize, sample: usize) -> Option<f32> {
        let n = self.points.len();
        if n == 0 {
            return None;
        }
        if upper == 0 {
            return Some(self.points[0].1);
        }
        if upper >= n {
            return Some(self.points[n - 1].1);
        }
        let (s0, v0) = self.points[upper - 1];
        let (s1, v1) = self.points[upper];
        if s1 <= s0 {
            return Some(v1);
        }
        let t = (sample - s0) as f64 / (s1 - s0) as f64;
        Some(interpolate(self.scale, v0, v1, t))
    }
}

fn interpolate(scale: Scale, a: f32, b: f32, t: f64) -> f32 {
    match scale {
        Scale::Linear => (f64::from(a) + (f64::from(b) - f64::from(a)) * t) as f32,
        Scale::Log => {
            let (la, lb) = (
                f64::from(a.max(LOG_FLOOR)).ln(),
                f64::from(b.max(LOG_FLOOR)).ln(),
            );
            (la + (lb - la) * t).exp() as f32
        }
    }
}

/// Evaluates one curve at positions that mostly increase, in amortised
/// constant time; a step backwards falls back to a search.
#[derive(Clone, Copy, Debug, Default)]
pub struct CurveCursor {
    upper: usize,
    last: usize,
}

impl CurveCursor {
    pub fn value_at(&mut self, curve: &Curve, sample: usize) -> Option<f32> {
        if sample < self.last {
            self.upper = curve.points.partition_point(|(s, _)| *s <= sample);
        } else {
            while self.upper < curve.points.len() && curve.points[self.upper].0 <= sample {
                self.upper += 1;
            }
        }
        self.last = sample;
        curve.value_with_upper(self.upper, sample)
    }
}

/// A tool's automated parameters and their curves.
#[derive(Clone, Debug, PartialEq)]
pub struct Tracks<P> {
    pub tracks: Vec<(P, Curve)>,
}

impl<P> Default for Tracks<P> {
    fn default() -> Self {
        Self { tracks: Vec::new() }
    }
}

impl<P: Copy + PartialEq> Tracks<P> {
    /// Nothing automated.
    pub fn none() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// Automate `param` along `curve`. A curve with no points is no track.
    pub fn push(&mut self, param: P, curve: Curve) {
        if !curve.points.is_empty() {
            self.tracks.push((param, curve));
        }
    }

    pub fn contains(&self, param: P) -> bool {
        self.tracks.iter().any(|(p, _)| *p == param)
    }

    pub fn curve(&self, param: P) -> Option<&Curve> {
        self.tracks
            .iter()
            .find(|(p, _)| *p == param)
            .map(|(_, c)| c)
    }

    pub fn params(&self) -> impl Iterator<Item = P> + '_ {
        self.tracks.iter().map(|(p, _)| *p)
    }
}

/// Settings some of whose fields can follow a curve.
pub trait Automatable: Clone + PartialEq {
    type Param: Copy + PartialEq + Debug;
    /// Set `param` to `value`, kept in the parameter's range.
    fn set_param(&mut self, param: Self::Param, value: f32);
    /// The value `param` has now.
    fn param(&self, param: Self::Param) -> f32;
}

/// A tool's settings over time: its static settings with every automated
/// parameter replaced by its curve's value.
pub struct Evaluator<'a, S: Automatable> {
    base: S,
    tracks: &'a Tracks<S::Param>,
    cursors: Vec<CurveCursor>,
}

impl<'a, S: Automatable> Evaluator<'a, S> {
    pub fn new(base: S, tracks: &'a Tracks<S::Param>) -> Self {
        Self {
            base,
            cursors: vec![CurveCursor::default(); tracks.tracks.len()],
            tracks,
        }
    }

    /// The settings in force at `sample`.
    pub fn at(&mut self, sample: usize) -> S {
        let mut settings = self.base.clone();
        for ((param, curve), cursor) in self.tracks.tracks.iter().zip(self.cursors.iter_mut()) {
            if let Some(value) = cursor.value_at(curve, sample) {
                settings.set_param(*param, value);
            }
        }
        settings
    }
}

/// The settings `base` and `tracks` describe at `sample`, without a cursor:
/// for a picture of one moment (the Inspector at the playhead).
pub fn settings_at<S: Automatable>(base: &S, tracks: &Tracks<S::Param>, sample: usize) -> S {
    let mut settings = base.clone();
    for (param, curve) in &tracks.tracks {
        if let Some(value) = curve.value_at(sample) {
            settings.set_param(*param, value);
        }
    }
    settings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct Two {
        a: f32,
        b: f32,
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum TwoParam {
        A,
        B,
    }

    impl Automatable for Two {
        type Param = TwoParam;
        fn set_param(&mut self, param: TwoParam, value: f32) {
            match param {
                TwoParam::A => self.a = value,
                TwoParam::B => self.b = value,
            }
        }
        fn param(&self, param: TwoParam) -> f32 {
            match param {
                TwoParam::A => self.a,
                TwoParam::B => self.b,
            }
        }
    }

    #[test]
    fn a_linear_curve_interpolates_and_holds_its_ends() {
        let c = Curve::new(vec![(100, -12.0), (200, 0.0)], Scale::Linear);
        assert_eq!(c.value_at(0), Some(-12.0));
        assert_eq!(c.value_at(100), Some(-12.0));
        assert_eq!(c.value_at(150), Some(-6.0));
        assert_eq!(c.value_at(200), Some(0.0));
        assert_eq!(c.value_at(10_000), Some(0.0));
        assert_eq!(Curve::new(Vec::new(), Scale::Linear).value_at(5), None);
    }

    #[test]
    fn a_log_curve_sweeps_by_ratio() {
        let c = Curve::new(vec![(0, 100.0), (100, 10_000.0)], Scale::Log);
        let mid = c.value_at(50).unwrap();
        assert!((mid - 1_000.0).abs() < 0.1, "{mid}");
        let quarter = c.value_at(25).unwrap();
        assert!((quarter - 316.227_77).abs() < 0.05, "{quarter}");
    }

    #[test]
    fn points_are_sorted_whatever_order_they_come_in() {
        let c = Curve::new(vec![(200, 1.0), (0, 0.0)], Scale::Linear);
        assert_eq!(c.points, vec![(0, 0.0), (200, 1.0)]);
        assert_eq!(c.min_value(), Some(0.0));
    }

    #[test]
    fn the_cursor_agrees_with_a_search_forwards_and_back() {
        let c = Curve::new(
            vec![(10, 1.0), (20, 5.0), (40, 2.0), (41, 9.0), (90, 3.0)],
            Scale::Linear,
        );
        let mut cursor = CurveCursor::default();
        for s in (0..120).chain((0..120).rev()).chain([30, 5, 95, 41, 40]) {
            assert_eq!(cursor.value_at(&c, s), c.value_at(s), "at {s}");
        }
    }

    #[test]
    fn the_evaluator_replaces_only_the_automated_fields() {
        let base = Two { a: 1.0, b: 2.0 };
        let mut tracks = Tracks::none();
        tracks.push(
            TwoParam::B,
            Curve::new(vec![(0, 10.0), (10, 20.0)], Scale::Linear),
        );
        tracks.push(TwoParam::A, Curve::new(Vec::new(), Scale::Linear));
        assert_eq!(tracks.tracks.len(), 1, "an empty curve is no track");
        let mut eval = Evaluator::new(base.clone(), &tracks);
        assert_eq!(eval.at(5), Two { a: 1.0, b: 15.0 });
        assert_eq!(settings_at(&base, &tracks, 20), Two { a: 1.0, b: 20.0 });
        assert_eq!(eval.at(20).param(TwoParam::A), 1.0);
        assert!(tracks.contains(TwoParam::B) && !tracks.contains(TwoParam::A));
    }
}
