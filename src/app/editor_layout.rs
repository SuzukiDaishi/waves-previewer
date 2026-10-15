//! The editor's canvas column as rules: how its height divides between the
//! waveform, the time navigator and the mini meter, and where each channel
//! lane sits when there are more lanes than the waveform has room for.
//!
//! No UI and no state. `ui/editor.rs` asks these and draws; every place that
//! turns a lane into a y coordinate, or a y coordinate into a lane, asks the
//! same `LaneGeometry`, because two places that work it out separately are
//! two places that disagree once the lanes scroll.

use std::ops::Range;

/// The lowest a channel lane is drawn: enough room for the centre line and
/// the -6/-12 dBFS grid either side of it. Below this a lane's trace stops
/// reading as a waveform, so past it the lanes keep this height and scroll
/// instead of shrinking.
pub(crate) const MIN_LANE_H: f32 = 44.0;

/// The mini meter's height until the splitter is dragged. At the default
/// 1280 x 720 window this is about what the space left below the navigator
/// used to give the meter, so the waveform keeps its old size there.
pub(crate) const MINI_METER_DEFAULT_H: f32 = 180.0;

/// Below this the meter's panels have no room for their labels. The strip
/// used to be dropped under the same height.
pub(crate) const MINI_METER_MIN_H: f32 = 64.0;

/// The drag handle between the navigator and the meter: as thick as the
/// Inspector's width divider, the other splitter on this screen.
pub(crate) const METER_SPLITTER_H: f32 = 6.0;

/// The waveform is what the editor is for: the meter gives way before the
/// waveform drops under this, and in a column too short for even this much
/// (a small window, or the narrow layout that stacks the Inspector below)
/// the waveform keeps it anyway and the column runs past its bottom edge,
/// as the canvas always did. The canvas's old floor.
pub(crate) const MIN_WAVE_VIEW_H: f32 = 180.0;

/// Height of the time navigator's overview strip.
pub(crate) const TIME_NAV_H: f32 = 54.0;

/// The lane scrollbar at the canvas's right edge, and the gap left of it.
pub(crate) const LANE_SCROLLBAR_W: f32 = 10.0;
pub(crate) const LANE_SCROLLBAR_GAP: f32 = 4.0;

/// Lanes the spectral viewport renders beyond each edge of the view, so a
/// small scroll shows lanes that are already drawn while the next render runs.
pub(crate) const LANE_WINDOW_OVERSCAN: usize = 1;

/// The smallest scrollbar thumb: with hundreds of lanes the proportional size
/// would be a pixel or two, which nobody can grab.
const MIN_THUMB_H: f32 = 18.0;

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

/// How the canvas column's height is shared out, top to bottom: the
/// waveform, the time navigator (`nav_reserved_h`, not stored here), the
/// splitter and the meter. The meter sits on the column's bottom edge; the
/// waveform takes whatever the meter leaves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EditorColumnLayout {
    pub wave_h: f32,
    /// 0 when the column is too short for a meter at all.
    pub meter_h: f32,
    /// The tallest the meter may be in this column, so a drag past it does
    /// not leave a stored height the splitter has to be dragged back through.
    pub meter_max_h: f32,
}

impl EditorColumnLayout {
    pub fn shows_meter(&self) -> bool {
        self.meter_h > 0.0
    }
}

/// Split a column `column_h` tall. `meter_pref_h` is the height the user
/// asked for (by dragging the splitter); it gives way to the waveform's
/// minimum, and the meter disappears when even its own minimum does not fit.
/// The waveform never goes under `MIN_WAVE_VIEW_H`, even when that takes
/// more than the column has.
///
/// `max` then `min`, never `clamp`: a short column makes the upper bound
/// fall under the lower one, and `f32::clamp` panics on that.
pub(crate) fn editor_column_layout(
    column_h: f32,
    nav_reserved_h: f32,
    meter_pref_h: f32,
) -> EditorColumnLayout {
    let room = (finite_or(column_h, 0.0) - finite_or(nav_reserved_h, 0.0).max(0.0)).max(1.0);
    let meter_max_h = room - METER_SPLITTER_H - MIN_WAVE_VIEW_H;
    if meter_max_h < MINI_METER_MIN_H {
        return EditorColumnLayout {
            wave_h: room.max(MIN_WAVE_VIEW_H),
            meter_h: 0.0,
            meter_max_h: 0.0,
        };
    }
    let meter_h = finite_or(meter_pref_h, MINI_METER_DEFAULT_H)
        .max(MINI_METER_MIN_H)
        .min(meter_max_h);
    EditorColumnLayout {
        wave_h: room - METER_SPLITTER_H - meter_h,
        meter_h,
        meter_max_h,
    }
}

/// The meter's height after the splitter moved `dy` (down is positive, and
/// makes the meter shorter), kept inside what this column allows.
pub(crate) fn meter_height_after_drag(layout: &EditorColumnLayout, dy: f32) -> f32 {
    (layout.meter_h - finite_or(dy, 0.0))
        .max(MINI_METER_MIN_H)
        .min(layout.meter_max_h.max(MINI_METER_MIN_H))
}

/// Where the channel lanes are. The view is the waveform canvas; when the
/// lanes fit they share its height, and when they do not each is
/// `MIN_LANE_H` tall and the stack scrolls by `scroll_px`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LaneGeometry {
    pub view_top: f32,
    pub view_h: f32,
    pub lane_h: f32,
    pub lane_count: usize,
    pub scroll_px: f32,
}

impl LaneGeometry {
    pub fn new(view_top: f32, view_h: f32, lane_count: usize, scroll_px: f32) -> Self {
        let lane_count = lane_count.max(1);
        let view_h = finite_or(view_h, 1.0).max(1.0);
        let fit = view_h / lane_count as f32;
        let mut geometry = Self {
            view_top: finite_or(view_top, 0.0),
            view_h,
            lane_h: fit.max(MIN_LANE_H),
            lane_count,
            scroll_px: 0.0,
        };
        geometry.scroll_px = geometry.clamp_scroll(scroll_px);
        geometry
    }

    /// Whether the lanes are taller than the view, i.e. whether it scrolls.
    pub fn overflows(&self) -> bool {
        self.view_h / (self.lane_count as f32) < MIN_LANE_H
    }

    pub fn content_h(&self) -> f32 {
        self.lane_h * self.lane_count as f32
    }

    pub fn max_scroll(&self) -> f32 {
        if self.overflows() {
            (self.content_h() - self.view_h).max(0.0)
        } else {
            0.0
        }
    }

    pub fn clamp_scroll(&self, scroll_px: f32) -> f32 {
        finite_or(scroll_px, 0.0).max(0.0).min(self.max_scroll())
    }

    /// The scroll position `dy` further down, kept in range.
    pub fn scrolled_by(&self, dy: f32) -> f32 {
        self.clamp_scroll(self.scroll_px + finite_or(dy, 0.0))
    }

    /// Where lane 0 would be drawn -- above the view once it has scrolled.
    pub fn lanes_top(&self) -> f32 {
        self.view_top - self.scroll_px
    }

    pub fn lane_top(&self, lane: usize) -> f32 {
        self.lanes_top() + self.lane_h * lane as f32
    }

    /// The lane under `y`; a point above or below the stack takes the nearest.
    pub fn lane_at_y(&self, y: f32) -> usize {
        // `as usize` saturates, and `f32::max` drops a NaN, so neither an
        // infinite nor a NaN `y` reaches the index unclamped.
        let lane = ((y - self.lanes_top()) / self.lane_h).floor().max(0.0) as usize;
        lane.min(self.lane_count - 1)
    }

    /// The lanes any part of which is inside the view. Never empty.
    pub fn visible_lanes(&self) -> Range<usize> {
        let first =
            ((self.scroll_px / self.lane_h).floor().max(0.0) as usize).min(self.lane_count - 1);
        let end = ((self.scroll_px + self.view_h) / self.lane_h)
            .ceil()
            .max(0.0) as usize;
        first..end.max(first + 1).min(self.lane_count)
    }

    /// `visible_lanes` widened by `overscan` lanes each way.
    pub fn window(&self, overscan: usize) -> Range<usize> {
        let visible = self.visible_lanes();
        visible.start.saturating_sub(overscan)..(visible.end + overscan).min(self.lane_count)
    }

    /// The scrollbar thumb inside a track `track_h` tall starting at
    /// `track_top`: its top and its height.
    pub fn thumb_span(&self, track_top: f32, track_h: f32) -> (f32, f32) {
        let track_h = finite_or(track_h, 0.0).max(0.0);
        let share = self.view_h / self.content_h().max(1.0);
        let thumb_h = (track_h * share).max(MIN_THUMB_H).min(track_h);
        let travel = (track_h - thumb_h).max(0.0);
        let max_scroll = self.max_scroll();
        let frac = if max_scroll > 0.0 {
            self.scroll_px / max_scroll
        } else {
            0.0
        };
        (track_top + travel * frac, thumb_h)
    }

    /// The scroll position after the thumb moved `dy` along a track
    /// `track_h` tall.
    pub fn scroll_after_thumb_drag(&self, dy: f32, track_h: f32) -> f32 {
        let (_, thumb_h) = self.thumb_span(0.0, track_h);
        let travel = (finite_or(track_h, 0.0) - thumb_h).max(1.0);
        self.scrolled_by(finite_or(dy, 0.0) / travel * self.max_scroll())
    }

    /// A click on the track off the thumb pages toward it, keeping one lane
    /// of the old view in the new one.
    pub fn scroll_after_track_click(&self, y: f32, track_top: f32, track_h: f32) -> f32 {
        let (thumb_top, thumb_h) = self.thumb_span(track_top, track_h);
        let page = (self.view_h - self.lane_h).max(self.lane_h);
        if y < thumb_top {
            self.scrolled_by(-page)
        } else if y > thumb_top + thumb_h {
            self.scrolled_by(page)
        } else {
            self.scroll_px
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAV: f32 = 80.0;

    #[test]
    fn the_meter_keeps_its_height_and_the_waveform_takes_the_rest() {
        let short = editor_column_layout(600.0, NAV, MINI_METER_DEFAULT_H);
        let tall = editor_column_layout(900.0, NAV, MINI_METER_DEFAULT_H);
        assert_eq!(short.meter_h, MINI_METER_DEFAULT_H);
        assert_eq!(tall.meter_h, MINI_METER_DEFAULT_H);
        assert_eq!(tall.wave_h - short.wave_h, 300.0);
        assert_eq!(short.wave_h + METER_SPLITTER_H + short.meter_h, 600.0 - NAV);
    }

    #[test]
    fn the_meter_gives_way_to_the_waveform_then_disappears() {
        let squeezed = editor_column_layout(400.0, NAV, 300.0);
        assert_eq!(squeezed.wave_h, MIN_WAVE_VIEW_H);
        assert!(squeezed.meter_h < 300.0 && squeezed.shows_meter());
        let too_short = editor_column_layout(
            NAV + MIN_WAVE_VIEW_H + METER_SPLITTER_H + MINI_METER_MIN_H - 1.0,
            NAV,
            MINI_METER_DEFAULT_H,
        );
        assert!(!too_short.shows_meter());
        assert_eq!(
            too_short.wave_h,
            MIN_WAVE_VIEW_H + METER_SPLITTER_H + MINI_METER_MIN_H - 1.0
        );
        // Shorter still, the waveform keeps its floor and the column
        // overflows rather than the canvas collapsing.
        let tiny = editor_column_layout(NAV + 100.0, NAV, MINI_METER_DEFAULT_H);
        assert!(!tiny.shows_meter());
        assert_eq!(tiny.wave_h, MIN_WAVE_VIEW_H);
    }

    #[test]
    fn the_meter_never_goes_under_its_minimum() {
        let layout = editor_column_layout(800.0, NAV, 10.0);
        assert_eq!(layout.meter_h, MINI_METER_MIN_H);
    }

    #[test]
    fn nonsense_heights_do_not_panic() {
        for (column, nav, pref) in [
            (0.0, 0.0, 0.0),
            (-50.0, 80.0, 180.0),
            (f32::NAN, 80.0, 180.0),
            (600.0, f32::INFINITY, f32::NAN),
            (f32::INFINITY, 80.0, f32::INFINITY),
        ] {
            let layout = editor_column_layout(column, nav, pref);
            assert!(layout.wave_h >= MIN_WAVE_VIEW_H, "{layout:?}");
            assert!(layout.meter_h >= 0.0, "{layout:?}");
        }
    }

    #[test]
    fn a_splitter_drag_stays_inside_the_column() {
        let layout = editor_column_layout(700.0, NAV, MINI_METER_DEFAULT_H);
        assert_eq!(
            meter_height_after_drag(&layout, -100.0),
            MINI_METER_DEFAULT_H + 100.0
        );
        assert_eq!(
            meter_height_after_drag(&layout, -10_000.0),
            layout.meter_max_h
        );
        assert_eq!(meter_height_after_drag(&layout, 10_000.0), MINI_METER_MIN_H);
    }

    #[test]
    fn lanes_that_fit_share_the_view_and_do_not_scroll() {
        let lanes = LaneGeometry::new(100.0, 400.0, 4, 250.0);
        assert!(!lanes.overflows());
        assert_eq!(lanes.lane_h, 100.0);
        assert_eq!(lanes.scroll_px, 0.0);
        assert_eq!(lanes.lane_top(2), 300.0);
        assert_eq!(lanes.visible_lanes(), 0..4);
        assert_eq!(lanes.lane_at_y(399.0), 2);
    }

    #[test]
    fn lanes_that_do_not_fit_keep_their_height_and_scroll() {
        let lanes = LaneGeometry::new(0.0, 440.0, 26, 0.0);
        assert!(lanes.overflows());
        assert_eq!(lanes.lane_h, MIN_LANE_H);
        assert_eq!(lanes.max_scroll(), 26.0 * MIN_LANE_H - 440.0);
        assert_eq!(lanes.visible_lanes(), 0..10);
        let scrolled = LaneGeometry::new(0.0, 440.0, 26, 3.5 * MIN_LANE_H);
        assert_eq!(scrolled.visible_lanes(), 3..14);
        assert_eq!(scrolled.window(1), 2..15);
        assert_eq!(scrolled.lane_at_y(0.0), 3);
        assert_eq!(scrolled.lane_top(4), 0.5 * MIN_LANE_H);
    }

    #[test]
    fn scrolling_stops_at_either_end() {
        let lanes = LaneGeometry::new(0.0, 440.0, 26, 1e9);
        assert_eq!(lanes.scroll_px, lanes.max_scroll());
        assert_eq!(lanes.visible_lanes().end, 26);
        assert!(lanes.visible_lanes().contains(&25));
        assert_eq!(lanes.scrolled_by(1e9), lanes.max_scroll());
        assert_eq!(lanes.scrolled_by(-1e9), 0.0);
        assert_eq!(LaneGeometry::new(0.0, 440.0, 26, -5.0).scroll_px, 0.0);
        assert_eq!(LaneGeometry::new(0.0, 440.0, 26, f32::NAN).scroll_px, 0.0);
    }

    #[test]
    fn hundreds_of_lanes_show_only_a_handful() {
        let lanes = LaneGeometry::new(0.0, 400.0, 1024, 500.0 * MIN_LANE_H);
        let visible = lanes.visible_lanes();
        assert_eq!(visible.start, 500);
        assert!(visible.len() <= (400.0 / MIN_LANE_H).ceil() as usize + 1);
        assert_eq!(lanes.lane_at_y(-1e9), 0);
        assert_eq!(lanes.lane_at_y(1e9), 1023);
        assert_eq!(lanes.lane_at_y(f32::NAN), 0);
    }

    #[test]
    fn the_thumb_follows_the_scroll_and_stays_grabbable() {
        let lanes = LaneGeometry::new(0.0, 440.0, 1024, 0.0);
        let (top, h) = lanes.thumb_span(10.0, 400.0);
        assert_eq!((top, h), (10.0, MIN_THUMB_H));
        let bottom = LaneGeometry::new(0.0, 440.0, 1024, 1e9);
        let (top, h) = bottom.thumb_span(10.0, 400.0);
        assert_eq!(top + h, 410.0);
        // Dragging the thumb the whole track scrolls the whole content.
        assert_eq!(
            lanes.scroll_after_thumb_drag(400.0, 400.0),
            lanes.max_scroll()
        );
        let half = LaneGeometry::new(0.0, 440.0, 20, 0.0);
        let (_, h) = half.thumb_span(0.0, 440.0);
        assert_eq!(h, 440.0 * 440.0 / (20.0 * MIN_LANE_H));
    }

    #[test]
    fn a_track_click_pages_toward_the_click() {
        let lanes = LaneGeometry::new(0.0, 440.0, 26, 0.0);
        let page = 440.0 - MIN_LANE_H;
        assert_eq!(lanes.scroll_after_track_click(430.0, 0.0, 440.0), page);
        let down = LaneGeometry::new(0.0, 440.0, 26, page);
        assert_eq!(down.scroll_after_track_click(1.0, 0.0, 440.0), 0.0);
        let (thumb_top, thumb_h) = down.thumb_span(0.0, 440.0);
        assert_eq!(
            down.scroll_after_track_click(thumb_top + thumb_h * 0.5, 0.0, 440.0),
            page
        );
    }
}
