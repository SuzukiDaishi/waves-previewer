use chrono::Local;
use regex::Regex;
use std::{
    backtrace::Backtrace,
    env, fs, io,
    path::{Path, PathBuf},
    process,
    sync::{
        atomic::{AtomicU8, Ordering},
        Once,
    },
};

const ENV_REPORT_DIR: &str = "NEOWAVES_CRASH_REPORT_DIR";

static HOOK_INSTALLED: Once = Once::new();
static APP_MODE: AtomicU8 = AtomicU8::new(CrashReportMode::Unknown as u8);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CrashReportMode {
    Unknown = 0,
    Gui = 1,
    Cli = 2,
}

impl CrashReportMode {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Gui,
            2 => Self::Cli,
            _ => Self::Unknown,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Gui => "gui",
            Self::Cli => "cli",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrashReportEntry {
    pub id: String,
    pub path: PathBuf,
    pub created_at: String,
    pub summary: String,
}

struct ReportParts {
    mode: CrashReportMode,
    panic_message: String,
    location: String,
    thread_name: String,
    args: Vec<String>,
    backtrace: Option<String>,
}

pub fn install_panic_hook(app_mode: CrashReportMode) {
    APP_MODE.store(app_mode as u8, Ordering::SeqCst);
    HOOK_INSTALLED.call_once(|| {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let mode = CrashReportMode::from_u8(APP_MODE.load(Ordering::SeqCst));
            match write_panic_report(info, mode) {
                Ok(path) => {
                    eprintln!("NeoWaves crash report saved: {}", path.display());
                }
                Err(err) => {
                    eprintln!("NeoWaves crash report could not be saved: {err}");
                }
            }
            previous_hook(info);
        }));
    });
}

pub fn write_panic_report(
    info: &std::panic::PanicHookInfo<'_>,
    mode: CrashReportMode,
) -> io::Result<PathBuf> {
    let thread = std::thread::current();
    let location = info
        .location()
        .map(|location| {
            format!(
                "{}:{}:{}",
                anonymize_text(location.file()),
                location.line(),
                location.column()
            )
        })
        .unwrap_or_else(|| "unknown".to_owned());

    let parts = ReportParts {
        mode,
        panic_message: panic_payload_message(info.payload()),
        location,
        thread_name: thread.name().unwrap_or("unnamed").to_owned(),
        args: sanitized_args(),
        backtrace: Some(Backtrace::force_capture().to_string()),
    };
    write_report_from_parts(parts)
}

pub fn crash_report_dir() -> PathBuf {
    if let Some(path) = env::var_os(ENV_REPORT_DIR).filter(|value| !value.is_empty()) {
        return PathBuf::from(path);
    }

    if cfg!(feature = "kittest") {
        return env::temp_dir()
            .join("NeoWaves")
            .join("crash-reports-kittest");
    }

    if let Some(path) = env::var_os("APPDATA").filter(|value| !value.is_empty()) {
        return PathBuf::from(path).join("NeoWaves").join("crash-reports");
    }

    if let Some(path) = env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty()) {
        return PathBuf::from(path).join("NeoWaves").join("crash-reports");
    }

    env::temp_dir().join("NeoWaves").join("crash-reports")
}

pub fn list_unacknowledged_reports() -> io::Result<Vec<CrashReportEntry>> {
    let dir = crash_report_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut reports = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if !id.starts_with("crash_") || reviewed_marker_path(&dir, id).exists() {
            continue;
        }
        reports.push(report_entry_from_path(id.to_owned(), path)?);
    }
    reports.sort_by(|left, right| right.id.cmp(&left.id));
    Ok(reports)
}

pub fn acknowledge_report(id: &str) -> io::Result<()> {
    validate_report_id(id)?;
    let dir = crash_report_dir();
    fs::create_dir_all(&dir)?;
    fs::write(reviewed_marker_path(&dir, id), b"reviewed\n")
}

pub fn copyable_report_text(path: &Path) -> io::Result<String> {
    fs::read_to_string(path)
}

fn write_report_from_parts(parts: ReportParts) -> io::Result<PathBuf> {
    let dir = crash_report_dir();
    fs::create_dir_all(&dir)?;

    let now = Local::now();
    let id = format!("crash_{}_{}", now.format("%Y%m%d_%H%M%S"), process::id());
    let path = dir.join(format!("{id}.md"));
    let args = if parts.args.is_empty() {
        "(none)".to_owned()
    } else {
        parts.args.join(" ")
    };

    let mut report = String::new();
    report.push_str("# NeoWaves Crash Report\n\n");
    push_field(&mut report, "Report ID", &id);
    push_field(
        &mut report,
        "Created At",
        &now.format("%Y-%m-%d %H:%M:%S %:z").to_string(),
    );
    push_field(&mut report, "NeoWaves Version", env!("CARGO_PKG_VERSION"));
    push_field(&mut report, "Mode", parts.mode.as_str());
    push_field(&mut report, "Thread", &anonymize_text(&parts.thread_name));
    push_field(
        &mut report,
        "Panic Location",
        &anonymize_text(&parts.location),
    );
    push_field(
        &mut report,
        "Panic Message",
        &anonymize_text(&parts.panic_message),
    );
    push_field(
        &mut report,
        "Target",
        &format!("{}-{}", env::consts::OS, env::consts::ARCH),
    );
    push_field(
        &mut report,
        "Build",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
    );

    report.push_str("\n## Launch Arguments\n\n");
    report.push_str("```text\n");
    report.push_str(&anonymize_text(&args));
    report.push_str("\n```\n");

    if let Some(backtrace) = parts
        .backtrace
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("<disabled>"))
    {
        report.push_str("\n## Backtrace\n\n");
        report.push_str("```text\n");
        report.push_str(&anonymize_text(backtrace));
        report.push_str("\n```\n");
    }

    fs::write(&path, report)?;
    Ok(path)
}

fn report_entry_from_path(id: String, path: PathBuf) -> io::Result<CrashReportEntry> {
    let text = fs::read_to_string(&path)?;
    let created_at = markdown_field(&text, "Created At").unwrap_or_else(|| "unknown".to_owned());
    let summary =
        markdown_field(&text, "Panic Message").unwrap_or_else(|| "(no summary)".to_owned());
    Ok(CrashReportEntry {
        id,
        path,
        created_at,
        summary,
    })
}

fn markdown_field(text: &str, name: &str) -> Option<String> {
    let prefix = format!("- **{name}:** ");
    text.lines().find_map(|line| {
        line.strip_prefix(&prefix)
            .map(|value| value.trim().to_owned())
    })
}

fn push_field(report: &mut String, name: &str, value: &str) {
    report.push_str("- **");
    report.push_str(name);
    report.push_str(":** ");
    report.push_str(value.trim());
    report.push('\n');
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

fn sanitized_args() -> Vec<String> {
    env::args().skip(1).map(|arg| sanitize_arg(&arg)).collect()
}

fn sanitize_arg(arg: &str) -> String {
    if let Some((flag, value)) = arg.split_once('=') {
        if flag.starts_with('-') {
            return format!("{flag}={}", sanitize_arg_value(value));
        }
    }
    if arg.starts_with('-') {
        return arg.to_owned();
    }
    sanitize_arg_value(arg)
}

fn sanitize_arg_value(value: &str) -> String {
    if looks_like_path(value) {
        anonymize_path_token(value)
    } else {
        anonymize_text(value)
    }
}

fn looks_like_path(value: &str) -> bool {
    value.contains('\\')
        || value.contains('/')
        || value.contains(':')
        || Path::new(value).extension().is_some()
}

fn anonymize_text(text: &str) -> String {
    let windows_path = Regex::new(r#"(?i)[a-z]:\\[^\s`"'<>\]\)]+(?:[^\s`"'<>\]\),.;:]*)"#)
        .expect("valid windows path regex");
    let unix_path =
        Regex::new(r#"/[^\s`"'<>\]\)]+(?:/[^\s`"'<>\]\)]+)+"#).expect("valid unix path regex");

    let text = windows_path
        .replace_all(text, |captures: &regex::Captures<'_>| {
            anonymize_path_token(&captures[0])
        })
        .into_owned();
    unix_path
        .replace_all(&text, |captures: &regex::Captures<'_>| {
            anonymize_path_token(&captures[0])
        })
        .into_owned()
}

fn anonymize_path_token(token: &str) -> String {
    let trimmed = token.trim_matches(|c: char| {
        matches!(
            c,
            '"' | '\'' | '`' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}'
        )
    });
    let trimmed = trimmed.trim_end_matches(|c: char| matches!(c, ',' | ';' | '.'));
    let normalized = trimmed.replace('\\', "/");
    let mut file_name = normalized
        .rsplit('/')
        .find(|part| !part.is_empty())
        .filter(|part| part.contains('.'))
        .unwrap_or_default()
        .to_owned();
    for _ in 0..2 {
        let Some((name, suffix)) = file_name.rsplit_once(':') else {
            break;
        };
        if suffix.chars().all(|ch| ch.is_ascii_digit()) {
            file_name = name.to_owned();
        } else {
            break;
        }
    }
    if file_name.is_empty() {
        "<path>".to_owned()
    } else {
        format!("<path:{file_name}>")
    }
}

fn reviewed_marker_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.reviewed"))
}

fn validate_report_id(id: &str) -> io::Result<()> {
    let valid = id.starts_with("crash_")
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'));
    if valid {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid crash report id",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    struct EnvGuard {
        key: &'static str,
        old_value: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &Path) -> Self {
            let old_value = env::var_os(key);
            env::set_var(key, value);
            Self { key, old_value }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = self.old_value.as_ref() {
                env::set_var(self.key, value);
            } else {
                env::remove_var(self.key);
            }
        }
    }

    fn temp_report_dir(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "neowaves_crash_report_test_{name}_{}_{}",
            process::id(),
            Local::now().timestamp_nanos_opt().unwrap_or_default()
        ))
    }

    fn with_report_dir<T>(name: &str, f: impl FnOnce(&Path) -> T) -> T {
        let _lock = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let dir = temp_report_dir(name);
        let _guard = EnvGuard::set(ENV_REPORT_DIR, &dir);
        let result = f(&dir);
        let _ = fs::remove_dir_all(&dir);
        result
    }

    #[test]
    fn crash_report_dir_uses_appdata_then_localappdata_then_fallback() {
        let _lock = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let temp = temp_report_dir("dir_resolution");
        let appdata = temp.join("appdata");
        let local = temp.join("localappdata");

        let report_guard = EnvGuard {
            key: ENV_REPORT_DIR,
            old_value: env::var_os(ENV_REPORT_DIR),
        };
        env::remove_var(ENV_REPORT_DIR);
        let app_guard = EnvGuard {
            key: "APPDATA",
            old_value: env::var_os("APPDATA"),
        };
        let local_guard = EnvGuard {
            key: "LOCALAPPDATA",
            old_value: env::var_os("LOCALAPPDATA"),
        };

        env::set_var("APPDATA", &appdata);
        env::set_var("LOCALAPPDATA", &local);
        assert_eq!(
            crash_report_dir(),
            appdata.join("NeoWaves").join("crash-reports")
        );

        env::remove_var("APPDATA");
        assert_eq!(
            crash_report_dir(),
            local.join("NeoWaves").join("crash-reports")
        );

        env::remove_var("LOCALAPPDATA");
        assert_eq!(
            crash_report_dir(),
            env::temp_dir().join("NeoWaves").join("crash-reports")
        );

        drop(local_guard);
        drop(app_guard);
        drop(report_guard);
        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn report_markdown_contains_panic_payload_and_location() {
        with_report_dir("markdown", |_| {
            let path = write_report_from_parts(ReportParts {
                mode: CrashReportMode::Gui,
                panic_message: "boom".to_owned(),
                location: "src/app.rs:10:2".to_owned(),
                thread_name: "main".to_owned(),
                args: vec!["--debug".to_owned()],
                backtrace: None,
            })
            .unwrap();
            let text = fs::read_to_string(path).unwrap();
            assert!(text.contains("- **Panic Message:** boom"));
            assert!(text.contains("- **Panic Location:** src/app.rs:10:2"));
            assert!(text.contains("- **Mode:** gui"));
        });
    }

    #[test]
    fn report_anonymizes_full_paths_and_user_names() {
        with_report_dir("anonymize", |_| {
            let path = write_report_from_parts(ReportParts {
                mode: CrashReportMode::Cli,
                panic_message:
                    r#"failed C:\Users\Alice\Music\secret.wav and /home/bob/takes/song.wav"#
                        .to_owned(),
                location: r#"C:\Users\Alice\repo\src\main.rs:1:1"#.to_owned(),
                thread_name: "main".to_owned(),
                args: vec![r#"<path:input.wav>"#.to_owned()],
                backtrace: None,
            })
            .unwrap();
            let text = fs::read_to_string(path).unwrap();
            assert!(!text.contains("Alice"));
            assert!(!text.contains("/home/bob"));
            assert!(!text.contains(r#"C:\Users"#));
            assert!(text.contains("<path:secret.wav>"));
            assert!(text.contains("<path:song.wav>"));
            assert!(text.contains("<path:main.rs>"));
        });
    }

    #[test]
    fn acknowledge_report_removes_it_from_unacknowledged_list() {
        with_report_dir("ack", |_| {
            let path = write_report_from_parts(ReportParts {
                mode: CrashReportMode::Gui,
                panic_message: "review me".to_owned(),
                location: "src/app.rs:1:1".to_owned(),
                thread_name: "main".to_owned(),
                args: Vec::new(),
                backtrace: None,
            })
            .unwrap();
            let id = path.file_stem().unwrap().to_string_lossy().to_string();
            assert_eq!(list_unacknowledged_reports().unwrap().len(), 1);
            acknowledge_report(&id).unwrap();
            assert!(list_unacknowledged_reports().unwrap().is_empty());
        });
    }
}
