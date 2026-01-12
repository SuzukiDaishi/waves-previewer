use egui::{text::LayoutJob, text::TextFormat, Color32, FontId, RichText, TextStyle};
use regex::RegexBuilder;

use super::types::{SortDir, SortKey};

pub fn db_to_amp(db: f32) -> f32 {
    if db <= -80.0 {
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

pub fn amp_to_color(a: f32) -> Color32 {
    let t = a.clamp(0.0, 1.0).powf(0.6); // emphasize loud parts
    lerp_color(
        Color32::from_rgb(80, 200, 255),
        Color32::from_rgb(255, 70, 70),
        t,
    )
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
            SortDir::Asc => " ▲",
            SortDir::Desc => " ▼",
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

pub fn num_order(a: f32, b: f32) -> std::cmp::Ordering {
    a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
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

pub fn highlight_text_job(
    text: &str,
    query: &str,
    use_regex: bool,
    style: &egui::Style,
) -> Option<LayoutJob> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    let re = if use_regex {
        RegexBuilder::new(q).case_insensitive(true).build().ok()?
    } else {
        RegexBuilder::new(&regex::escape(q))
            .case_insensitive(true)
            .build()
            .ok()?
    };
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

#[allow(dead_code)]
pub fn open_folder_with_file_selected(file_path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        // Windows: /select パラメータでファイルを選択状態でフォルダを開く
        Command::new("explorer")
            .arg("/select,")
            .arg(file_path)
            .spawn()?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        // macOS: -R フラグでFinderでファイルを選択状態で開く
        Command::new("open").arg("-R").arg(file_path).spawn()?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use std::process::Command;
        // Linux: ファイルマネージャーでフォルダを開く (ファイル選択は一般的にサポートされていない)
        if let Some(parent) = file_path.parent() {
            Command::new("xdg-open").arg(parent).spawn()?;
        }
        Ok(())
    }
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
