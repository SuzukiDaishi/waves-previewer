use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

pub struct ExternalTable {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub sheet_names: Vec<String>,
    pub sheet_name: Option<String>,
}

pub enum ExternalLoadMsg {
    Progress { rows: usize },
    Done(Result<ExternalTable, String>),
}

pub struct ExternalLoadConfig {
    pub path: PathBuf,
    pub sheet_name: Option<String>,
    pub has_header: bool,
    /// 0-based. None = auto-detect when has_header = true.
    pub header_row: Option<usize>,
    /// 0-based. None = auto (header_row + 1) or 0 when no header.
    pub data_row: Option<usize>,
}

#[allow(dead_code)]
pub fn load_table(path: &Path) -> Option<ExternalTable> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "csv" => load_csv(path),
        _ => None,
    }
}

pub fn spawn_load_table(cfg: ExternalLoadConfig, tx: Sender<ExternalLoadMsg>) {
    std::thread::spawn(move || {
        let res = (|| -> Result<ExternalTable, String> {
            let ext = cfg
                .path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            match ext.as_str() {
                "csv" => load_csv_with_progress(&cfg, &tx),
                "xlsx" | "xls" => load_excel_with_progress(&cfg, &tx),
                _ => Err("Unsupported data source.".to_string()),
            }
        })();
        let _ = tx.send(ExternalLoadMsg::Done(res));
    });
}

#[allow(dead_code)]
fn load_csv(path: &Path) -> Option<ExternalTable> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_path(path)
        .ok()?;
    let headers_record = rdr.headers().ok()?.clone();
    let mut headers: Vec<String> = headers_record
        .iter()
        .enumerate()
        .map(|(i, h)| normalize_header(h, i))
        .collect();
    if headers.is_empty() {
        return None;
    }
    let mut rows: Vec<Vec<String>> = Vec::new();
    for result in rdr.records() {
        let record = result.ok()?;
        let mut row = vec![String::new(); headers.len()];
        for (idx, val) in record.iter().enumerate() {
            if idx < row.len() {
                row[idx] = val.trim().to_string();
            }
        }
        rows.push(row);
    }
    if headers.iter().all(|h| h.is_empty()) {
        headers = (0..headers.len())
            .map(|i| format!("Column{}", i + 1))
            .collect();
    }
    Some(ExternalTable {
        headers,
        rows,
        sheet_names: Vec::new(),
        sheet_name: None,
    })
}

fn load_csv_with_progress(
    cfg: &ExternalLoadConfig,
    tx: &Sender<ExternalLoadMsg>,
) -> Result<ExternalTable, String> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_path(&cfg.path)
        .map_err(|e| format!("csv open failed: {e}"))?;
    let mut raw_rows: Vec<Vec<String>> = Vec::new();
    let mut row_count = 0usize;
    for result in rdr.records() {
        let record = result.map_err(|e| format!("csv read failed: {e}"))?;
        let mut row = Vec::with_capacity(record.len());
        for val in record.iter() {
            row.push(val.trim().to_string());
        }
        raw_rows.push(row);
        row_count += 1;
        if row_count % 1000 == 0 {
            let _ = tx.send(ExternalLoadMsg::Progress { rows: row_count });
        }
    }
    let (headers, rows) = build_table_from_rows(&raw_rows, cfg);
    let _ = tx.send(ExternalLoadMsg::Progress { rows: row_count });
    Ok(ExternalTable {
        headers,
        rows,
        sheet_names: Vec::new(),
        sheet_name: None,
    })
}

fn normalize_header(raw: &str, idx: usize) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        format!("Column{}", idx + 1)
    } else {
        trimmed.to_string()
    }
}

fn load_excel_with_progress(
    cfg: &ExternalLoadConfig,
    tx: &Sender<ExternalLoadMsg>,
) -> Result<ExternalTable, String> {
    use calamine::{open_workbook_auto, Reader};
    let mut workbook = open_workbook_auto(&cfg.path)
        .map_err(|e| format!("excel open failed: {e}"))?;
    let sheet_names = workbook.sheet_names().to_vec();
    let sheet = if let Some(name) = cfg.sheet_name.as_ref() {
        name.clone()
    } else {
        sheet_names
            .get(0)
            .cloned()
            .ok_or_else(|| "No sheets found.".to_string())?
    };
    let range = workbook
        .worksheet_range(&sheet)
        .map_err(|e| format!("sheet read failed: {e}"))?;
    let mut raw_rows: Vec<Vec<String>> = Vec::new();
    let mut row_count = 0usize;
    for row in range.rows() {
        let mut out = Vec::with_capacity(row.len());
        for cell in row {
            out.push(cell_to_string(cell));
        }
        raw_rows.push(out);
        row_count += 1;
        if row_count % 1000 == 0 {
            let _ = tx.send(ExternalLoadMsg::Progress { rows: row_count });
        }
    }
    let (headers, rows) = build_table_from_rows(&raw_rows, cfg);
    Ok(ExternalTable {
        headers,
        rows,
        sheet_names,
        sheet_name: Some(sheet),
    })
}

fn cell_to_string(cell: &calamine::Data) -> String {
    use calamine::Data;
    match cell {
        Data::Empty => String::new(),
        Data::String(s) => s.trim().to_string(),
        Data::Float(f) => {
            if f.fract() == 0.0 {
                format!("{:.0}", f)
            } else {
                format!("{:.4}", f)
            }
        }
        Data::Int(i) => i.to_string(),
        Data::Bool(b) => b.to_string(),
        Data::DateTime(f) => format!("{:.4}", f),
        Data::DateTimeIso(s) => s.trim().to_string(),
        Data::DurationIso(s) => s.trim().to_string(),
        Data::Error(e) => format!("{:?}", e),
    }
}

fn build_table_from_rows(
    raw_rows: &[Vec<String>],
    cfg: &ExternalLoadConfig,
) -> (Vec<String>, Vec<Vec<String>>) {
    if raw_rows.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let max_cols = raw_rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut header_row = cfg.header_row;
    let mut has_header = cfg.has_header;
    if has_header && header_row.is_none() {
        header_row = detect_header_row(raw_rows);
        if header_row.is_none() {
            has_header = false;
        }
    }
    let data_row = cfg.data_row.or_else(|| {
        if has_header {
            header_row.map(|r| r + 1)
        } else {
            Some(0)
        }
    });
    let start_row = data_row.unwrap_or(0);
    let headers = if has_header {
        if let Some(idx) = header_row {
            raw_rows
                .get(idx)
                .map(|row| {
                    (0..max_cols)
                        .map(|i| row.get(i).map(|s| s.as_str()).unwrap_or(""))
                        .enumerate()
                        .map(|(i, h)| normalize_header(h, i))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| (0..max_cols).map(|i| format!("Column{}", i + 1)).collect())
        } else {
            (0..max_cols).map(|i| format!("Column{}", i + 1)).collect()
        }
    } else {
        (0..max_cols).map(|i| format!("Column{}", i + 1)).collect()
    };
    let mut rows: Vec<Vec<String>> = Vec::new();
    for (idx, row) in raw_rows.iter().enumerate() {
        if idx < start_row {
            continue;
        }
        let mut out = vec![String::new(); headers.len()];
        for (col_idx, val) in row.iter().enumerate() {
            if col_idx < out.len() {
                out[col_idx] = val.trim().to_string();
            }
        }
        rows.push(out);
    }
    (headers, rows)
}

fn detect_header_row(raw_rows: &[Vec<String>]) -> Option<usize> {
    let scan = raw_rows.len().min(50);
    if scan == 0 {
        return None;
    }
    let mut best_idx = None;
    let mut best_score = 0.0f32;
    for i in 0..scan {
        let row = &raw_rows[i];
        if row.is_empty() {
            continue;
        }
        let mut non_empty = 0usize;
        let mut texty = 0usize;
        let mut numeric = 0usize;
        let mut unique = std::collections::HashSet::new();
        for cell in row {
            let v = cell.trim();
            if v.is_empty() {
                continue;
            }
            non_empty += 1;
            if v.chars().any(|c| c.is_alphabetic()) {
                texty += 1;
            } else if v.parse::<f64>().is_ok() {
                numeric += 1;
            }
            unique.insert(v.to_ascii_lowercase());
        }
        if non_empty == 0 {
            continue;
        }
        let uniq_ratio = unique.len() as f32 / non_empty as f32;
        let text_ratio = texty as f32 / non_empty as f32;
        let num_ratio = numeric as f32 / non_empty as f32;
        let score = uniq_ratio * 0.5 + text_ratio * 0.7 - num_ratio * 0.3;
        if score > best_score {
            best_score = score;
            best_idx = Some(i);
        }
    }
    if best_score < 0.2 {
        None
    } else {
        best_idx
    }
}
