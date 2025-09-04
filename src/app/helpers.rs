use egui::{RichText, Color32};

use super::types::{SortDir, SortKey};

pub fn db_to_amp(db: f32) -> f32 {
    if db <= -80.0 { 0.0 } else { (10.0f32).powf(db/20.0) }
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
        (0.0,   Color32::from_rgb(255, 150, 60)), // orange
        (6.0,   Color32::from_rgb(255, 70, 70)),  // red (near 0 dBFS+)
    ];
    let x = db.clamp(pts.first().unwrap().0, pts.last().unwrap().0);
    // find segment
    for w in pts.windows(2) {
        let (x0, c0) = w[0];
        let (x1, c1) = w[1];
        if x >= x0 && x <= x1 {
            let t = if (x1 - x0).abs() < f32::EPSILON { 0.0 } else { (x - x0) / (x1 - x0) };
            return lerp_color(c0, c1, t);
        }
    }
    pts.last().unwrap().1
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0,1.0);
    let r = (a.r() as f32 + (b.r() as f32 - a.r() as f32)*t) as u8;
    let g = (a.g() as f32 + (b.g() as f32 - a.g() as f32)*t) as u8;
    let bl = (a.b() as f32 + (b.b() as f32 - a.b() as f32)*t) as u8;
    Color32::from_rgb(r,g,bl)
}

pub fn amp_to_color(a: f32) -> Color32 {
    let t = a.clamp(0.0, 1.0).powf(0.6); // emphasize loud parts
    lerp_color(Color32::from_rgb(80,200,255), Color32::from_rgb(255,70,70), t)
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
    let arrow = if is_active { match *sort_dir { SortDir::Asc => " ▲", SortDir::Desc => " ▼", SortDir::None => "" } } else { "" };
    let btn = egui::Button::new(RichText::new(format!("{}{}", label, arrow)).strong());
    let clicked = ui.add(btn).clicked();
    if clicked {
        if *sort_key != key {
            *sort_key = key;
            *sort_dir = if default_asc { SortDir::Asc } else { SortDir::Desc };
        } else {
            *sort_dir = match *sort_dir { SortDir::Asc => SortDir::Desc, SortDir::Desc => SortDir::None, SortDir::None => if default_asc { SortDir::Asc } else { SortDir::Desc } };
        }
        return true;
    }
    false
}

pub fn num_order(a: f32, b: f32) -> std::cmp::Ordering {
    a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
}

pub fn format_duration(secs: f32) -> String {
    let s = if secs.is_finite() && secs >= 0.0 { secs } else { 0.0 };
    let total = s.round() as u64;
    let m = total / 60;
    let s = total % 60;
    format!("{}:{:02}", m, s)
}

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

pub fn open_folder_with_file_selected(file_path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        // Windows: /select パラメータでファイルを選択状態でフォルダを開く
        Command::new("explorer").arg("/select,").arg(file_path).spawn()?;
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

