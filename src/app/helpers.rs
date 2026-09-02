use egui::{text::LayoutJob, text::TextFormat, Color32, FontId, RichText, TextStyle};
use regex::RegexBuilder;

use super::types::{SortDir, SortKey};

pub const GAIN_DB_MIN: f32 = -80.0;
pub const GAIN_DB_MAX: f32 = 24.0;

/// Monitor volume range, in dB. A little above unity so a quiet file can be
/// pushed up without leaving the fader.
pub const VOLUME_DB_MIN: f32 = -80.0;
pub const VOLUME_DB_MAX: f32 = 6.0;

/// Where each dB value sits along the volume fader, bottom to top.
///
/// A fader that is linear in dB spends nearly two thirds of its travel below
/// -24 dB, where the only decision left is "off", and squeezes the range
/// people actually monitor in -- unity down to -24 -- into a quarter of it.
/// Every small adjustment then lands in a few pixels. These anchors give that
/// range a bit under half the fader and compress the tail toward silence, the
/// way a console fader's taper does.
///
/// The mapping stays continuous and strictly increasing, so the fader remains
/// stepless: every position resolves to exactly one dB value, and every dB
/// value to exactly one position.
const VOLUME_TAPER: &[(f32, f32)] = &[
    (0.00, VOLUME_DB_MIN),
    (0.22, -48.0),
    (0.45, -24.0),
    (0.90, 0.0),
    (1.00, VOLUME_DB_MAX),
];

/// The dB value at fader position `t` (0 at the bottom, 1 at the top).
pub fn volume_db_from_fader(t: f32) -> f32 {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    for pair in VOLUME_TAPER.windows(2) {
        let (t0, db0) = pair[0];
        let (t1, db1) = pair[1];
        if t <= t1 {
            let span = t1 - t0;
            let frac = if span > 0.0 { (t - t0) / span } else { 0.0 };
            return db0 + (db1 - db0) * frac;
        }
    }
    VOLUME_DB_MAX
}

/// The fader position for `db`. The inverse of [`volume_db_from_fader`].
pub fn volume_fader_from_db(db: f32) -> f32 {
    let db = if db.is_finite() {
        db.clamp(VOLUME_DB_MIN, VOLUME_DB_MAX)
    } else {
        VOLUME_DB_MIN
    };
    for pair in VOLUME_TAPER.windows(2) {
        let (t0, db0) = pair[0];
        let (t1, db1) = pair[1];
        if db <= db1 {
            let span = db1 - db0;
            let frac = if span > 0.0 { (db - db0) / span } else { 0.0 };
            return (t0 + (t1 - t0) * frac).clamp(0.0, 1.0);
        }
    }
    1.0
}

pub fn db_to_amp(db: f32) -> f32 {
    if db <= GAIN_DB_MIN {
        0.0
    } else {
        (10.0f32).powf(db / 20.0)
    }
}

pub fn db_to_color(db: f32) -> Color32 {
    // Expanded palette for clearer perception across ranges.
    // Control points: (dBFS, Color)
    let pts: &[(f32, Color32)] = &[
        (-80.0, Color32::from_rgb(10, 10, 12)),   // near silence
        (-60.0, Color32::from_rgb(20, 50, 110)),  // deep blue
        (-40.0, Color32::from_rgb(40, 100, 180)), // blue
        (-25.0, Color32::from_rgb(80, 200, 255)), // cyan/teal
        (-12.0, Color32::from_rgb(220, 220, 60)), // yellow
        (0.0, Color32::from_rgb(255, 150, 60)),   // orange
        (6.0, Color32::from_rgb(255, 70, 70)),    // red (near 0 dBFS+)
    ];
    let x = db.clamp(pts.first().unwrap().0, pts.last().unwrap().0);
    // find segment
    for w in pts.windows(2) {
        let (x0, c0) = w[0];
        let (x1, c1) = w[1];
        if x >= x0 && x <= x1 {
            let t = if (x1 - x0).abs() < f32::EPSILON {
                0.0
            } else {
                (x - x0) / (x1 - x0)
            };
            return lerp_color(c0, c1, t);
        }
    }
    pts.last().unwrap().1
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let r = (a.r() as f32 + (b.r() as f32 - a.r() as f32) * t) as u8;
    let g = (a.g() as f32 + (b.g() as f32 - a.g() as f32) * t) as u8;
    let bl = (a.b() as f32 + (b.b() as f32 - a.b() as f32) * t) as u8;
    Color32::from_rgb(r, g, bl)
}

/// Waveform trace colour for a column whose peak amplitude is `a`.
///
/// Quiet is cool, loud is hot, and the amber stop in the middle is what makes
/// the ramp readable: cyan interpolated straight to red crosses a desaturated
/// mauve right where most material sits, and — worse — it got *darker* as it
/// got louder (the old endpoints were luminance 170 quiet, 125 loud), so a
/// waveform's peaks were the dimmest thing drawn. Brightness now rises with
/// amplitude across almost the whole range and only eases off at the top,
/// where the colour turns to warn about level.
///
/// This is the editor canvas *and* the list's Wave column thumbnails.
const WAVE_QUIET: Color32 = Color32::from_rgb(72, 206, 250);
const WAVE_MID: Color32 = Color32::from_rgb(250, 208, 84);
const WAVE_LOUD: Color32 = Color32::from_rgb(255, 124, 96);
/// Where `WAVE_MID` sits on the ramp. Past halfway, so ordinary material —
/// which lands around t = 0.25..0.66 after the `powf` below — is drawn with
/// the brightest part of the ramp rather than the crossing between stops.
const WAVE_MID_T: f32 = 0.62;

/// Record a click, and say whether it was the second of a pair in the same spot.
///
/// egui's own double-click reporting does not survive contact with either of
/// the surfaces that need it here. `Response::double_clicked` never fires on a
/// canvas that also senses drags, because the second press is taken for the
/// start of one; and `PointerState::button_double_clicked` uses a 300 ms window
/// that is tighter than these surfaces are repainted. The editor's note rows
/// and the amplitude navigator each grew their own version of this check for
/// the same reason -- this is that check, written once.
///
/// A repeat clears the state, so a third click starts a new pair rather than
/// counting as a second double.
pub fn note_repeated_click(
    state: &mut Option<(std::time::Instant, egui::Pos2)>,
    pos: egui::Pos2,
) -> bool {
    const WINDOW: std::time::Duration = std::time::Duration::from_millis(400);
    const SLOP_PX: f32 = 6.0;
    let now = std::time::Instant::now();
    let repeat = state.is_some_and(|(at, prev)| {
        now.saturating_duration_since(at) <= WINDOW && prev.distance(pos) <= SLOP_PX
    });
    *state = if repeat { None } else { Some((now, pos)) };
    repeat
}

pub fn amp_to_color(a: f32) -> Color32 {
    let t = a.clamp(0.0, 1.0).powf(0.6); // emphasize loud parts
    if t <= WAVE_MID_T {
        lerp_color(WAVE_QUIET, WAVE_MID, t / WAVE_MID_T)
    } else {
        lerp_color(WAVE_MID, WAVE_LOUD, (t - WAVE_MID_T) / (1.0 - WAVE_MID_T))
    }
}

/// Rec.601 perceived luminance, 0..255. Only used to hold the waveform ramp
/// to a brightness floor in tests.
#[cfg(test)]
mod volume_taper_tests {
    use super::*;

    #[test]
    fn the_fader_is_continuous_and_strictly_increasing() {
        let mut previous = f32::NEG_INFINITY;
        for step in 0..=1000 {
            let db = volume_db_from_fader(step as f32 / 1000.0);
            assert!(
                db > previous - 1.0e-4,
                "the fader must never go backwards: {db} after {previous}"
            );
            // A jump would mean values the fader cannot reach.
            if previous > f32::NEG_INFINITY {
                assert!(db - previous < 1.0, "step too coarse at {db}");
            }
            previous = db;
        }
        assert!((volume_db_from_fader(0.0) - VOLUME_DB_MIN).abs() < 1.0e-4);
        assert!((volume_db_from_fader(1.0) - VOLUME_DB_MAX).abs() < 1.0e-4);
    }

    #[test]
    fn a_position_and_its_db_value_round_trip() {
        for step in 0..=200 {
            let t = step as f32 / 200.0;
            let round_tripped = volume_fader_from_db(volume_db_from_fader(t));
            assert!(
                (round_tripped - t).abs() < 1.0e-3,
                "position {t} came back as {round_tripped}"
            );
        }
    }

    #[test]
    fn the_monitoring_range_gets_the_room() {
        // The point of the taper: unity down to -24 dB is where the adjustment
        // actually happens, and it must not be a sliver of the fader. Linear
        // in dB it would be 24/86 -- under a third.
        let span = volume_fader_from_db(0.0) - volume_fader_from_db(-24.0);
        assert!(
            span > 0.40,
            "0..-24 dB should own most of the fader, got {span}"
        );
        // ...without pushing unity so far up that the top is unreachable.
        assert!(volume_fader_from_db(0.0) < 0.95);
    }

    #[test]
    fn out_of_range_and_nonsense_values_are_pinned() {
        assert!((volume_db_from_fader(-1.0) - VOLUME_DB_MIN).abs() < 1.0e-4);
        assert!((volume_db_from_fader(2.0) - VOLUME_DB_MAX).abs() < 1.0e-4);
        assert!((volume_db_from_fader(f32::NAN) - VOLUME_DB_MIN).abs() < 1.0e-4);
        assert!(volume_fader_from_db(-200.0) < 1.0e-4);
        assert!(volume_fader_from_db(200.0) > 1.0 - 1.0e-4);
        assert!(volume_fader_from_db(f32::NAN) < 1.0e-4);
    }
}

#[cfg(test)]
fn perceived_luminance(c: Color32) -> f32 {
    0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32
}

pub fn sortable_header(
    ui: &mut egui::Ui,
    label: &str,
    sort_key: &mut SortKey,
    sort_dir: &mut SortDir,
    key: SortKey,
    default_asc: bool,
) -> bool {
    let is_active = *sort_key == key && *sort_dir != SortDir::None;
    let arrow = if is_active {
        match *sort_dir {
            SortDir::Asc => " \u{25B2}",
            SortDir::Desc => " \u{25BC}",
            SortDir::None => "",
        }
    } else {
        ""
    };
    let btn = egui::Button::new(RichText::new(format!("{}{}", label, arrow)).strong());
    let clicked = ui.add(btn).clicked();
    if clicked {
        if *sort_key != key {
            *sort_key = key;
            *sort_dir = if default_asc {
                SortDir::Asc
            } else {
                SortDir::Desc
            };
        } else {
            *sort_dir = match *sort_dir {
                SortDir::Asc => {
                    if default_asc {
                        SortDir::Desc
                    } else {
                        SortDir::None
                    }
                }
                SortDir::Desc => {
                    if default_asc {
                        SortDir::None
                    } else {
                        SortDir::Asc
                    }
                }
                SortDir::None => {
                    if default_asc {
                        SortDir::Asc
                    } else {
                        SortDir::Desc
                    }
                }
            };
        }
        return true;
    }
    false
}

pub fn format_duration(secs: f32) -> String {
    let s = if secs.is_finite() && secs >= 0.0 {
        secs
    } else {
        0.0
    };
    let total = s.round() as u64;
    let m = total / 60;
    let s = total % 60;
    format!("{}:{:02}", m, s)
}

/// `h:mm:ss`, used list-wide once any loaded file reaches an hour so a 2 h
/// file reads "2:00:11" instead of `format_duration`'s "120:11".
pub fn format_duration_hms(secs: f32) -> String {
    let s = if secs.is_finite() && secs >= 0.0 {
        secs
    } else {
        0.0
    };
    // Same rounding as format_duration, and as the >= 1 h test that picks
    // between them, so 3599.7 s can't render as "60:00" in one and
    // "1:00:00" in the other.
    let total = s.round() as u64;
    format!(
        "{}:{:02}:{:02}",
        total / 3600,
        (total % 3600) / 60,
        total % 60
    )
}

pub fn format_duration_scaled(secs: f32, use_hours: bool) -> String {
    if use_hours {
        format_duration_hms(secs)
    } else {
        format_duration(secs)
    }
}

// Compact time string with tenths when useful, e.g. 0:01.2, 1:23.4, 12:34.5
pub fn format_time_s(secs: f32) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "0:00.0".to_string();
    }
    let m = (secs / 60.0).floor() as u64;
    let s = secs - (m as f32) * 60.0;
    if m < 100 {
        // typical range
        format!("{}:{:04.1}", m, s)
    } else {
        // fallback: no decimals for very long
        format!("{}:{:02}", m, s.floor() as u64)
    }
}

pub fn format_system_time_local(st: std::time::SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Local> = st.into();
    dt.format("%Y-%m-%d %H:%M").to_string()
}

/// Compile the search highlight regex once; reuse via
/// `WavesPreviewer::cached_highlight_regex()` instead of rebuilding per label.
pub fn build_highlight_regex(query: &str, use_regex: bool) -> Option<regex::Regex> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    if use_regex {
        RegexBuilder::new(q).case_insensitive(true).build().ok()
    } else {
        RegexBuilder::new(&regex::escape(q))
            .case_insensitive(true)
            .build()
            .ok()
    }
}

#[allow(dead_code)]
pub fn highlight_text_job(
    text: &str,
    query: &str,
    use_regex: bool,
    style: &egui::Style,
) -> Option<LayoutJob> {
    let re = build_highlight_regex(query, use_regex)?;
    highlight_text_job_with_regex(text, &re, style)
}

pub fn highlight_text_job_with_regex(
    text: &str,
    re: &regex::Regex,
    style: &egui::Style,
) -> Option<LayoutJob> {
    let mut matches = Vec::new();
    for m in re.find_iter(text) {
        matches.push((m.start(), m.end()));
    }
    if matches.is_empty() {
        return None;
    }
    let font_id = style
        .text_styles
        .get(&TextStyle::Body)
        .cloned()
        .unwrap_or_else(|| FontId::proportional(14.0));
    let normal = TextFormat {
        font_id: font_id.clone(),
        color: style.visuals.text_color(),
        ..Default::default()
    };
    let highlight = TextFormat {
        font_id,
        color: Color32::from_rgb(255, 200, 80),
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    let mut last = 0;
    for (s, e) in matches {
        if s > last {
            job.append(&text[last..s], 0.0, normal.clone());
        }
        job.append(&text[s..e], 0.0, highlight.clone());
        last = e;
    }
    if last < text.len() {
        job.append(&text[last..], 0.0, normal);
    }
    Some(job)
}

#[allow(dead_code)]
pub fn open_in_file_explorer(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        Command::new("explorer").arg(path).spawn()?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        Command::new("open").arg(path).spawn()?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use std::process::Command;
        Command::new("xdg-open").arg(path).spawn()?;
        Ok(())
    }
}

/// Program + args used to reveal `file_path` in the OS file browser.
/// Windows: `explorer /select,` opens the folder with the file selected;
/// macOS: `open -R`; Linux: the parent folder via `xdg-open` (file
/// selection is generally unsupported there). `None` when there is no
/// usable target (a Linux path with no parent).
pub fn reveal_in_folder_command(
    file_path: &std::path::Path,
) -> Option<(&'static str, Vec<std::ffi::OsString>)> {
    #[cfg(target_os = "windows")]
    {
        Some((
            "explorer",
            vec!["/select,".into(), file_path.as_os_str().to_os_string()],
        ))
    }
    #[cfg(target_os = "macos")]
    {
        Some((
            "open",
            vec!["-R".into(), file_path.as_os_str().to_os_string()],
        ))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        file_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| ("xdg-open", vec![p.as_os_str().to_os_string()]))
    }
}

#[allow(dead_code)]
pub fn open_folder_with_file_selected(file_path: &std::path::Path) -> std::io::Result<()> {
    use std::process::Command;
    if let Some((program, args)) = reveal_in_folder_command(file_path) {
        Command::new(program).args(args).spawn()?;
    }
    Ok(())
}

// Sanitize a filename component for Windows: replace forbidden chars
// For simplicity, we replace <>:"/\|?* with '_' and trim trailing dots/spaces.
// Also avoid reserved names like CON, PRN, AUX, NUL, COM1..COM9, LPT1..LPT9 by appending '_'.
pub fn sanitize_filename_component(name: &str) -> String {
    // Replace forbidden characters using a raw string
    let forbidden: &str = r#"<>:"/\|?*"#;
    let mut s: String = name
        .chars()
        .map(|c| if forbidden.contains(c) { '_' } else { c })
        .collect();
    // Trim trailing dots/spaces
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    if s.is_empty() {
        s = "untitled".to_string();
    }
    // Avoid reserved names
    let upper = s.to_ascii_uppercase();
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&upper.as_str()) {
        s.push('_');
    }
    s
}

#[cfg(test)]
mod duration_tests {
    use super::{format_duration, format_duration_hms, format_duration_scaled};

    #[test]
    fn hms_splits_hours_out_of_the_minute_field() {
        assert_eq!(format_duration_hms(0.0), "0:00:00");
        assert_eq!(format_duration_hms(3.4), "0:00:03");
        assert_eq!(format_duration_hms(67.5), "0:01:08");
        // The case that motivated the format: format_duration says "120:11".
        assert_eq!(format_duration(7211.0), "120:11");
        assert_eq!(format_duration_hms(7211.0), "2:00:11");
    }

    #[test]
    fn hms_rounds_the_same_way_the_one_hour_test_does() {
        // 3599.7 rounds to 3600, so the latch (>= 3600 after rounding) and the
        // formatter must agree — "1:00:00", never "60:00".
        assert_eq!(format_duration_hms(3599.7), "1:00:00");
    }

    #[test]
    fn hms_clamps_non_finite_and_negative() {
        assert_eq!(format_duration_hms(f32::NAN), "0:00:00");
        assert_eq!(format_duration_hms(-5.0), "0:00:00");
    }

    #[test]
    fn scaled_picks_the_format() {
        assert_eq!(format_duration_scaled(67.5, false), "1:08");
        assert_eq!(format_duration_scaled(67.5, true), "0:01:08");
    }
}

#[cfg(test)]
mod reveal_tests {
    use super::reveal_in_folder_command;
    use std::path::Path;

    #[test]
    fn reveal_command_is_well_formed() {
        let cmd = reveal_in_folder_command(Path::new("/tmp/somewhere/file.wav"));
        let (program, args) = cmd.expect("command for a normal path");
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            assert_eq!(program, "xdg-open");
            assert_eq!(args, vec![std::ffi::OsString::from("/tmp/somewhere")]);
        }
        #[cfg(target_os = "macos")]
        {
            assert_eq!(program, "open");
            assert_eq!(args[0], std::ffi::OsString::from("-R"));
        }
        #[cfg(target_os = "windows")]
        {
            assert_eq!(program, "explorer");
            assert_eq!(args[0], std::ffi::OsString::from("/select,"));
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn reveal_command_none_without_parent() {
        assert!(reveal_in_folder_command(Path::new("/")).is_none());
    }
}

#[cfg(test)]
mod waveform_color_tests {
    use super::{amp_to_color, perceived_luminance};

    /// Largest difference between any two channels: 0 is a pure grey.
    fn chroma(c: egui::Color32) -> f32 {
        let (r, g, b) = (c.r() as f32, c.g() as f32, c.b() as f32);
        r.max(g).max(b) - r.min(g).min(b)
    }

    #[test]
    fn the_ramp_never_crosses_through_grey() {
        // The ramp this replaced ran cyan straight to red, whose midpoint is
        // rgb(167, 135, 162) -- chroma 32, a mauve. That crossing is where
        // ordinary material sits, so the busiest part of a waveform was also
        // the least legible.
        let mut worst = f32::INFINITY;
        for step in 0..=200 {
            let amp = step as f32 / 200.0;
            worst = worst.min(chroma(amp_to_color(amp)));
        }
        assert!(worst >= 40.0, "waveform ramp desaturates to chroma {worst}");
    }

    #[test]
    fn peaks_are_not_the_dimmest_thing_on_screen() {
        // The old ramp ended at rgb(255, 70, 70) -- luminance 125, darker than
        // its own quiet end at 170. Amplitude and brightness ran in opposite
        // directions, so the loudest columns receded instead of standing out.
        let mut worst = f32::INFINITY;
        for step in 0..=200 {
            let amp = step as f32 / 200.0;
            worst = worst.min(perceived_luminance(amp_to_color(amp)));
        }
        assert!(worst >= 155.0, "waveform ramp dims to luminance {worst}");
    }

    #[test]
    fn ordinary_material_lands_on_the_brightest_part_of_the_ramp() {
        // A column peaking around -9 dBFS is the common case; it should be
        // drawn brighter than either end of the ramp, not between them.
        let ordinary = perceived_luminance(amp_to_color(0.35));
        assert!(ordinary > perceived_luminance(amp_to_color(0.0)));
        assert!(ordinary > perceived_luminance(amp_to_color(1.0)));
    }
}
