use std::path::Path;

pub struct ExternalTable {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

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
    Some(ExternalTable { headers, rows })
}

fn normalize_header(raw: &str, idx: usize) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        format!("Column{}", idx + 1)
    } else {
        trimmed.to_string()
    }
}
