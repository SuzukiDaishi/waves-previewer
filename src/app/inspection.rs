//! Batch QA inspection core: pure per-file checks shared by the GUI worker
//! pool and the CLI `batch inspect` command.
//!
//! All loudness/peak checks evaluate the *effective* value (measured +
//! pending list gain), matching what the list columns display. Rows for
//! passing files are kept in the result set so reports stay complete; the
//! results window filters them out by default.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InspectionConfig {
    pub check_true_peak: bool,
    /// Effective true peak (or sample peak fallback) above this warns.
    pub tp_ceiling_db: f32,
    pub check_loudness: bool,
    pub target_lufs: f32,
    pub lufs_tolerance_lu: f32,
    pub check_silence: bool,
    pub silence_threshold_dbfs: f32,
    pub max_leading_silence_ms: f32,
    pub max_trailing_silence_ms: f32,
    pub check_loop: bool,
    /// When set, a file without loop markers is flagged.
    pub require_loop: bool,
}

impl Default for InspectionConfig {
    fn default() -> Self {
        Self {
            check_true_peak: true,
            tp_ceiling_db: -1.0,
            check_loudness: true,
            target_lufs: -14.0,
            lufs_tolerance_lu: 1.0,
            check_silence: true,
            silence_threshold_dbfs: -60.0,
            max_leading_silence_ms: 100.0,
            max_trailing_silence_ms: 1000.0,
            check_loop: true,
            require_loop: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum IssueSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum InspectionIssueKind {
    DecodeError,
    TruePeakOver,
    LoudnessOutOfRange,
    LeadingSilence,
    TrailingSilence,
    LoopInvalid,
    LoopMissing,
}

#[derive(Clone, Debug, Serialize)]
pub struct InspectionIssue {
    pub kind: InspectionIssueKind,
    pub severity: IssueSeverity,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum LoopStatus {
    NotPresent,
    Valid,
    StartNotBeforeEnd,
    OutOfBounds,
}

/// One inspected file. Serialized directly into JSON/CSV reports.
#[derive(Clone, Debug, Serialize)]
pub struct InspectionRow {
    pub path: String,
    pub file: String,
    pub folder: String,
    pub pending_gain_db: f32,
    pub effective_lufs: Option<f32>,
    pub effective_true_peak_db: Option<f32>,
    /// True when `effective_true_peak_db` is a plain sample-peak fallback.
    pub true_peak_is_sample_peak: bool,
    pub leading_silence_ms: Option<f32>,
    pub trailing_silence_ms: Option<f32>,
    pub loop_status: LoopStatus,
    pub loop_points: Option<(u64, u64)>,
    pub total_frames: Option<u64>,
    pub decode_error: Option<String>,
    pub issues: Vec<InspectionIssue>,
    /// Max severity across issues; `None` = passed all enabled checks.
    pub severity: Option<IssueSeverity>,
}

/// Facts already known from the list's async metadata, so `inspect_file`
/// can skip decoding when every enabled check is answerable without it.
#[derive(Clone, Copy, Debug, Default)]
pub struct CachedAudioFacts {
    pub lufs_i: Option<f32>,
    pub true_peak_db: Option<f32>,
    /// Full-decode sample peak only (never the header-pass estimate).
    pub peak_db: Option<f32>,
    pub total_frames: Option<u64>,
}

/// Leading/trailing spans (ms) where every channel stays below
/// `threshold_dbfs`. A fully silent buffer reports the whole duration on
/// both ends.
pub fn scan_silence_ms(ch_samples: &[Vec<f32>], sample_rate: u32, threshold_dbfs: f32) -> (f32, f32) {
    let frames = ch_samples.iter().map(|c| c.len()).max().unwrap_or(0);
    let sr = sample_rate.max(1) as f32;
    if frames == 0 {
        return (0.0, 0.0);
    }
    let thresh = 10.0f32.powf(threshold_dbfs / 20.0);
    let loud_at = |i: usize| {
        ch_samples
            .iter()
            .any(|c| c.get(i).map(|v| v.abs() > thresh).unwrap_or(false))
    };
    let first_loud = (0..frames).find(|&i| loud_at(i));
    let Some(first) = first_loud else {
        let full_ms = frames as f32 * 1000.0 / sr;
        return (full_ms, full_ms);
    };
    let last = (0..frames).rev().find(|&i| loud_at(i)).unwrap_or(first);
    let lead_ms = first as f32 * 1000.0 / sr;
    let trail_ms = (frames - 1 - last) as f32 * 1000.0 / sr;
    (lead_ms, trail_ms)
}

/// Structural loop validation missing from the readers: bounds against the
/// file length in frames. Readers already refuse `end <= start` for most
/// formats, but sidecars and hand-edited chunks can still carry anything.
pub fn validate_loop(loop_points: Option<(u64, u64)>, total_frames: Option<u64>) -> LoopStatus {
    let Some((start, end)) = loop_points else {
        return LoopStatus::NotPresent;
    };
    if end <= start {
        return LoopStatus::StartNotBeforeEnd;
    }
    if let Some(frames) = total_frames {
        if end > frames || start >= frames {
            return LoopStatus::OutOfBounds;
        }
    }
    LoopStatus::Valid
}

#[allow(clippy::too_many_arguments)]
pub fn evaluate_checks(
    cfg: &InspectionConfig,
    effective_lufs: Option<f32>,
    effective_tp_db: Option<f32>,
    tp_is_sample_peak: bool,
    leading_silence_ms: Option<f32>,
    trailing_silence_ms: Option<f32>,
    loop_status: LoopStatus,
    decode_error: Option<&str>,
) -> Vec<InspectionIssue> {
    let mut issues = Vec::new();
    if let Some(err) = decode_error {
        issues.push(InspectionIssue {
            kind: InspectionIssueKind::DecodeError,
            severity: IssueSeverity::Error,
            message: format!("decode failed: {err}"),
        });
    }
    if cfg.check_true_peak {
        if let Some(tp) = effective_tp_db {
            if tp > cfg.tp_ceiling_db {
                let kind_note = if tp_is_sample_peak { " (sample peak)" } else { "" };
                issues.push(InspectionIssue {
                    kind: InspectionIssueKind::TruePeakOver,
                    severity: IssueSeverity::Warning,
                    message: format!(
                        "peak {tp:+.2} dBTP{kind_note} above ceiling {:+.1} dBTP",
                        cfg.tp_ceiling_db
                    ),
                });
            }
        }
    }
    if cfg.check_loudness {
        if let Some(lufs) = effective_lufs {
            let delta = lufs - cfg.target_lufs;
            if delta.abs() > cfg.lufs_tolerance_lu {
                issues.push(InspectionIssue {
                    kind: InspectionIssueKind::LoudnessOutOfRange,
                    severity: IssueSeverity::Warning,
                    message: format!(
                        "{lufs:+.1} LUFS is {delta:+.1} LU from target {:+.1} (tolerance ±{:.1})",
                        cfg.target_lufs, cfg.lufs_tolerance_lu
                    ),
                });
            }
        }
    }
    if cfg.check_silence {
        if let Some(lead) = leading_silence_ms {
            if lead > cfg.max_leading_silence_ms {
                issues.push(InspectionIssue {
                    kind: InspectionIssueKind::LeadingSilence,
                    severity: IssueSeverity::Warning,
                    message: format!(
                        "leading silence {lead:.0} ms exceeds {:.0} ms",
                        cfg.max_leading_silence_ms
                    ),
                });
            }
        }
        if let Some(trail) = trailing_silence_ms {
            if trail > cfg.max_trailing_silence_ms {
                issues.push(InspectionIssue {
                    kind: InspectionIssueKind::TrailingSilence,
                    severity: IssueSeverity::Warning,
                    message: format!(
                        "trailing silence {trail:.0} ms exceeds {:.0} ms",
                        cfg.max_trailing_silence_ms
                    ),
                });
            }
        }
    }
    if cfg.check_loop {
        match loop_status {
            LoopStatus::StartNotBeforeEnd => issues.push(InspectionIssue {
                kind: InspectionIssueKind::LoopInvalid,
                severity: IssueSeverity::Error,
                message: "loop end is not after loop start".to_string(),
            }),
            LoopStatus::OutOfBounds => issues.push(InspectionIssue {
                kind: InspectionIssueKind::LoopInvalid,
                severity: IssueSeverity::Error,
                message: "loop points fall outside the file".to_string(),
            }),
            LoopStatus::NotPresent if cfg.require_loop => issues.push(InspectionIssue {
                kind: InspectionIssueKind::LoopMissing,
                severity: IssueSeverity::Warning,
                message: "no loop markers (required)".to_string(),
            }),
            LoopStatus::NotPresent | LoopStatus::Valid => {}
        }
    }
    issues
}

fn row_severity(issues: &[InspectionIssue]) -> Option<IssueSeverity> {
    issues.iter().map(|i| i.severity).max()
}

/// Inspect one file. Decodes only when an enabled check needs data the
/// cached facts don't provide (silence always needs a decode).
pub fn inspect_file(
    path: &Path,
    pending_gain_db: f32,
    cached: &CachedAudioFacts,
    cfg: &InspectionConfig,
    cancel: &Arc<AtomicBool>,
) -> InspectionRow {
    let file = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let folder = path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    let mut lufs = cached.lufs_i;
    let mut tp_db = cached.true_peak_db;
    let mut sample_peak_db = cached.peak_db;
    let mut total_frames = cached.total_frames;
    let mut leading_ms = None;
    let mut trailing_ms = None;
    let mut decode_error: Option<String> = None;

    let needs_decode = cfg.check_silence
        || (cfg.check_loudness && lufs.is_none())
        || (cfg.check_true_peak && tp_db.is_none() && sample_peak_db.is_none());

    if needs_decode && !cancel.load(std::sync::atomic::Ordering::Relaxed) {
        match crate::audio_io::decode_audio_multi(path) {
            Ok((chans, sr)) => {
                let frames = chans.iter().map(|c| c.len()).max().unwrap_or(0);
                total_frames = total_frames.or(Some(frames as u64));
                if cfg.check_silence {
                    let (lead, trail) = scan_silence_ms(&chans, sr, cfg.silence_threshold_dbfs);
                    leading_ms = Some(lead);
                    trailing_ms = Some(trail);
                }
                if cfg.check_loudness && lufs.is_none() {
                    lufs = crate::wave::lufs_integrated_from_multi(&chans, sr).ok();
                }
                if cfg.check_true_peak && tp_db.is_none() && sample_peak_db.is_none() {
                    let peak = chans
                        .iter()
                        .flat_map(|c| c.iter())
                        .fold(0.0f32, |a, v| a.max(v.abs()));
                    if peak > 0.0 {
                        sample_peak_db = Some(20.0 * peak.log10());
                    } else {
                        sample_peak_db = Some(f32::NEG_INFINITY);
                    }
                }
            }
            Err(err) => decode_error = Some(err.to_string()),
        }
    }

    // Loop check never needs a decode: markers come from chunks/sidecars and
    // the length from the (cached or header) frame count.
    let loop_points = if cfg.check_loop {
        crate::loop_markers::read_loop_markers(path)
    } else {
        None
    };
    if cfg.check_loop && total_frames.is_none() {
        total_frames = crate::audio_io::read_audio_info(path)
            .ok()
            .and_then(|info| info.total_frames);
    }
    let loop_status = if cfg.check_loop {
        validate_loop(loop_points, total_frames)
    } else {
        LoopStatus::NotPresent
    };

    let effective_lufs = lufs.map(|v| v + pending_gain_db);
    let tp_is_sample_peak = tp_db.is_none();
    let effective_tp_db = tp_db
        .or(sample_peak_db)
        .filter(|v| v.is_finite())
        .map(|v| v + pending_gain_db);

    let issues = evaluate_checks(
        cfg,
        effective_lufs,
        effective_tp_db,
        tp_is_sample_peak,
        leading_ms,
        trailing_ms,
        loop_status,
        decode_error.as_deref(),
    );
    let severity = row_severity(&issues);

    InspectionRow {
        path: path.display().to_string(),
        file,
        folder,
        pending_gain_db,
        effective_lufs,
        effective_true_peak_db: effective_tp_db,
        true_peak_is_sample_peak: tp_is_sample_peak,
        leading_silence_ms: leading_ms,
        trailing_silence_ms: trailing_ms,
        loop_status,
        loop_points,
        total_frames,
        decode_error,
        issues,
        severity,
    }
}

fn severity_label(sev: Option<IssueSeverity>) -> &'static str {
    match sev {
        None => "pass",
        Some(IssueSeverity::Info) => "info",
        Some(IssueSeverity::Warning) => "warning",
        Some(IssueSeverity::Error) => "error",
    }
}

fn issues_summary(row: &InspectionRow) -> String {
    row.issues
        .iter()
        .map(|i| i.message.as_str())
        .collect::<Vec<_>>()
        .join("; ")
}

fn fmt_opt(v: Option<f32>) -> String {
    v.map(|v| format!("{v:.2}")).unwrap_or_default()
}

/// Write rows as JSON / CSV / TXT / Markdown depending on the extension
/// (same dispatch idea as the CLI batch-loudness report writer).
pub fn write_batch_inspection_report(
    path: &Path,
    rows: &[InspectionRow],
    cfg: &InspectionConfig,
) -> anyhow::Result<()> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "json" => {
            let body = serde_json::json!({
                "target_lufs": cfg.target_lufs,
                "lufs_tolerance_lu": cfg.lufs_tolerance_lu,
                "tp_ceiling_db": cfg.tp_ceiling_db,
                "rows": rows,
            });
            std::fs::write(path, serde_json::to_string_pretty(&body)?)?;
        }
        "csv" => {
            let mut out = String::new();
            out.push_str(
                "severity,file,folder,path,pending_gain_db,effective_lufs,effective_true_peak_db,\
                 leading_silence_ms,trailing_silence_ms,loop_status,loop_start,loop_end,\
                 total_frames,issues\n",
            );
            for row in rows {
                let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
                out.push_str(&format!(
                    "{},{},{},{},{:.2},{},{},{},{},{:?},{},{},{},{}\n",
                    severity_label(row.severity),
                    quote(&row.file),
                    quote(&row.folder),
                    quote(&row.path),
                    row.pending_gain_db,
                    fmt_opt(row.effective_lufs),
                    fmt_opt(row.effective_true_peak_db),
                    fmt_opt(row.leading_silence_ms),
                    fmt_opt(row.trailing_silence_ms),
                    row.loop_status,
                    row.loop_points.map(|(s, _)| s.to_string()).unwrap_or_default(),
                    row.loop_points.map(|(_, e)| e.to_string()).unwrap_or_default(),
                    row.total_frames.map(|v| v.to_string()).unwrap_or_default(),
                    quote(&issues_summary(row)),
                ));
            }
            std::fs::write(path, out)?;
        }
        "md" => {
            let mut out = String::from(
                "| Severity | File | LUFS | dBTP | Lead ms | Trail ms | Loop | Issues |\n\
                 |---|---|---|---|---|---|---|---|\n",
            );
            for row in rows {
                out.push_str(&format!(
                    "| {} | {} | {} | {} | {} | {} | {:?} | {} |\n",
                    severity_label(row.severity),
                    row.file,
                    fmt_opt(row.effective_lufs),
                    fmt_opt(row.effective_true_peak_db),
                    fmt_opt(row.leading_silence_ms),
                    fmt_opt(row.trailing_silence_ms),
                    row.loop_status,
                    issues_summary(row),
                ));
            }
            std::fs::write(path, out)?;
        }
        _ => {
            let mut out = String::new();
            for row in rows {
                out.push_str(&format!(
                    "[{}] {} — {}\n",
                    severity_label(row.severity),
                    row.path,
                    if row.issues.is_empty() {
                        "ok".to_string()
                    } else {
                        issues_summary(row)
                    }
                ));
            }
            std::fs::write(path, out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize, amp: f32) -> Vec<f32> {
        (0..frames)
            .map(|i| ((i as f32 / 48_000.0) * 440.0 * std::f32::consts::TAU).sin() * amp)
            .collect()
    }

    #[test]
    fn silence_scan_exact_boundaries() {
        // 100 ms silence + tone + 200 ms silence at 44.1 kHz.
        let sr = 44_100u32;
        let lead = (sr as usize) / 10;
        let trail = (sr as usize) / 5;
        let mut ch = vec![0.0f32; lead];
        ch.extend(tone(sr as usize, 0.5));
        ch.extend(vec![0.0f32; trail]);
        let (l, t) = scan_silence_ms(&[ch], sr, -60.0);
        assert!((l - 100.0).abs() < 2.0, "lead {l}");
        assert!((t - 200.0).abs() < 2.0, "trail {t}");

        // All-silent buffer: both ends report the full duration.
        let silent = vec![0.0f32; sr as usize];
        let (l, t) = scan_silence_ms(&[silent], sr, -60.0);
        assert!((l - 1000.0).abs() < 1.0);
        assert!((t - 1000.0).abs() < 1.0);
    }

    #[test]
    fn loop_validation_matrix() {
        assert_eq!(validate_loop(None, Some(1000)), LoopStatus::NotPresent);
        assert_eq!(validate_loop(Some((0, 500)), Some(1000)), LoopStatus::Valid);
        assert_eq!(
            validate_loop(Some((500, 500)), Some(1000)),
            LoopStatus::StartNotBeforeEnd
        );
        assert_eq!(
            validate_loop(Some((600, 400)), Some(1000)),
            LoopStatus::StartNotBeforeEnd
        );
        assert_eq!(
            validate_loop(Some((0, 1001)), Some(1000)),
            LoopStatus::OutOfBounds
        );
        assert_eq!(
            validate_loop(Some((1000, 1200)), Some(1000)),
            LoopStatus::OutOfBounds
        );
        // Unknown length: only structural checks apply.
        assert_eq!(validate_loop(Some((0, 10_000)), None), LoopStatus::Valid);
    }

    #[test]
    fn evaluate_checks_thresholds() {
        let cfg = InspectionConfig::default();
        // Exactly at target ± tolerance passes; just beyond warns.
        let ok = evaluate_checks(
            &cfg,
            Some(cfg.target_lufs + cfg.lufs_tolerance_lu),
            Some(cfg.tp_ceiling_db),
            false,
            Some(cfg.max_leading_silence_ms),
            Some(cfg.max_trailing_silence_ms),
            LoopStatus::Valid,
            None,
        );
        assert!(ok.is_empty(), "boundary values must pass: {ok:?}");

        let warn = evaluate_checks(
            &cfg,
            Some(cfg.target_lufs + cfg.lufs_tolerance_lu + 0.01),
            Some(cfg.tp_ceiling_db + 0.01),
            false,
            Some(cfg.max_leading_silence_ms + 1.0),
            Some(cfg.max_trailing_silence_ms + 1.0),
            LoopStatus::OutOfBounds,
            None,
        );
        let kinds: Vec<_> = warn.iter().map(|i| i.kind).collect();
        assert!(kinds.contains(&InspectionIssueKind::LoudnessOutOfRange));
        assert!(kinds.contains(&InspectionIssueKind::TruePeakOver));
        assert!(kinds.contains(&InspectionIssueKind::LeadingSilence));
        assert!(kinds.contains(&InspectionIssueKind::TrailingSilence));
        assert!(kinds.contains(&InspectionIssueKind::LoopInvalid));
        assert_eq!(super::row_severity(&warn), Some(IssueSeverity::Error));

        // require_loop flags a missing loop.
        let mut req = cfg;
        req.require_loop = true;
        let missing = evaluate_checks(
            &req,
            None,
            None,
            false,
            None,
            None,
            LoopStatus::NotPresent,
            None,
        );
        assert!(missing
            .iter()
            .any(|i| i.kind == InspectionIssueKind::LoopMissing));
    }

    #[test]
    fn inspect_file_decode_error_row() {
        let cfg = InspectionConfig::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let row = inspect_file(
            Path::new("/nonexistent/qa_missing.wav"),
            0.0,
            &CachedAudioFacts::default(),
            &cfg,
            &cancel,
        );
        assert!(row.decode_error.is_some());
        assert_eq!(row.severity, Some(IssueSeverity::Error));
        assert!(row
            .issues
            .iter()
            .any(|i| i.kind == InspectionIssueKind::DecodeError));
    }
}
