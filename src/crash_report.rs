use chrono::Local;
use regex::Regex;
use std::{
    backtrace::Backtrace,
    cell::Cell,
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

thread_local! {
    /// Set while a `catch_unwind` scope is responsible for the panic. The hook
    /// still runs (Rust has no way to skip it), so it reads this to decide
    /// whether the panic is worth a report on disk.
    static SUPPRESS_REPORTS: Cell<bool> = const { Cell::new(false) };
}

/// Restores the previous suppression state when dropped.
#[must_use = "the suppression only lasts while the guard is alive"]
pub struct PanicReportSuppression {
    previous: bool,
}

impl Drop for PanicReportSuppression {
    fn drop(&mut self) {
        SUPPRESS_REPORTS.with(|flag| flag.set(self.previous));
    }
}

/// Stop writing crash reports on this thread until the guard is dropped.
///
/// For panics the caller already handles — a `catch_unwind` that turns them
/// into an error the UI shows. Without this a recovered panic still leaves a
/// crash report behind, which reads to the user as a crash that never
/// happened. Panics still reach the default hook, so they stay visible on
/// stderr.
pub fn suppress_panic_reports() -> PanicReportSuppression {
    let previous = SUPPRESS_REPORTS.with(|flag| flag.replace(true));
    PanicReportSuppression { previous }
}

fn panic_reports_suppressed() -> bool {
    SUPPRESS_REPORTS.with(Cell::get)
}

/// Lets other modules' tests assert that they suppressed reporting.
#[cfg(test)]
pub fn panic_reports_suppressed_for_test() -> bool {
    panic_reports_suppressed()
}

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
            if panic_reports_suppressed() {
                previous_hook(info);
                return;
            }
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

/// The human-readable text of a panic payload, for both the report and
/// callers that recover from a panic themselves.
pub fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
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

    // Unix paths first: a replacement token can contain `/` (source-relative
    // paths keep their separators), and the Windows pattern needs a drive
    // letter followed by a backslash, so it can never match inside one. The
    // other order lets the Unix pass chew through tokens the Windows pass
    // just produced.
    let text = unix_path
        .replace_all(text, |captures: &regex::Captures<'_>| {
            anonymize_path_token(&captures[0])
        })
        .into_owned();
    windows_path
        .replace_all(&text, |captures: &regex::Captures<'_>| {
            anonymize_path_token(&captures[0])
        })
        .into_owned()
}

/// The part of a source path that is worth keeping in a report.
///
/// A bare file name is not enough to act on: a panic in `mod.rs` could belong
/// to any of hundreds of crates. The segments below are build-layout, not user
/// data — no account name, no directory choices — so keeping them costs no
/// privacy and turns "which `mod.rs`?" into an answer.
///
/// Everything else (the user's own files) still falls back to the file name.
fn source_relative_path(normalized: &str) -> Option<String> {
    if !normalized.ends_with(".rs") {
        return None;
    }
    // Dependency from crates.io: .../registry/src/<index>/<crate-version>/src/...
    if let Some(rest) = split_after(normalized, "/registry/src/") {
        // Drop the registry index host, keep the crate directory onwards.
        if let Some((_, crate_relative)) = rest.split_once('/') {
            return Some(crate_relative.to_owned());
        }
    }
    // Dependency from git: .../git/checkouts/<name-hash>/<rev>/src/...
    if let Some(rest) = split_after(normalized, "/git/checkouts/") {
        return Some(rest.to_owned());
    }
    // The standard library: /rustc/<hash>/library/std/src/...
    if let Some(rest) = split_after(normalized, "/library/") {
        if normalized.contains("/rustc/") {
            return Some(format!("library/{rest}"));
        }
    }
    // Our own crate: <build dir>/src/... — the build directory is the user's
    // choice, the path below `src/` is ours.
    split_after(normalized, "/src/").map(|rest| format!("src/{rest}"))
}

fn split_after<'a>(haystack: &'a str, marker: &str) -> Option<&'a str> {
    haystack
        .rfind(marker)
        .map(|at| &haystack[at + marker.len()..])
        .filter(|rest| !rest.is_empty())
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
    // Panic locations arrive as `<path>:<line>:<col>`. Strip those before
    // looking at the path so the source-relative match sees a real file name;
    // a Windows drive colon survives because its suffix is not all digits.
    let mut path = normalized.as_str();
    for _ in 0..2 {
        let Some((head, suffix)) = path.rsplit_once(':') else {
            break;
        };
        if suffix.chars().all(|ch| ch.is_ascii_digit()) {
            path = head;
        } else {
            break;
        }
    }
    let file_name = source_relative_path(path).unwrap_or_else(|| {
        path.rsplit('/')
            .find(|part| !part.is_empty())
            .filter(|part| part.contains('.'))
            .unwrap_or_default()
            .to_owned()
    });
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

    #[cfg(not(feature = "kittest"))]
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

    #[cfg(feature = "kittest")]
    #[test]
    fn crash_report_dir_isolated_from_user_profile_in_kittest() {
        let _lock = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let report_guard = EnvGuard {
            key: ENV_REPORT_DIR,
            old_value: env::var_os(ENV_REPORT_DIR),
        };
        env::remove_var(ENV_REPORT_DIR);

        assert_eq!(
            crash_report_dir(),
            env::temp_dir()
                .join("NeoWaves")
                .join("crash-reports-kittest")
        );

        drop(report_guard);
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
            // Source files keep the part below `src/`; the build directory
            // above it (which carries the user name) is still dropped.
            assert!(text.contains("<path:src/main.rs>"));
        });
    }

    #[test]
    fn anonymize_keeps_source_relative_paths_for_dependencies() {
        let registry = anonymize_text(
            r#"panicked at C:\Users\Alice\.cargo\registry\src\index.crates.io-1949cf8c\drag-2.1.1\src\platform_impl\windows\mod.rs:370:60"#,
        );
        assert!(
            registry.contains("<path:drag-2.1.1/src/platform_impl/windows/mod.rs>"),
            "{registry}"
        );
        assert!(!registry.contains("Alice"));

        let git = anonymize_text(
            "/home/bob/.cargo/git/checkouts/clack-e0a4d228e55f/f874e858/host/src/bundle/mod.rs:12:7",
        );
        assert!(
            git.contains("<path:clack-e0a4d228e55f/f874e858/host/src/bundle/mod.rs>"),
            "{git}"
        );
        assert!(!git.contains("bob"));

        let std_lib = anonymize_text("/rustc/9b00956e56/library/std/src/io/mod.rs:370:60");
        assert!(
            std_lib.contains("<path:library/std/src/io/mod.rs>"),
            "{std_lib}"
        );
    }

    #[test]
    fn anonymize_still_reduces_user_media_paths_to_a_file_name() {
        let text = anonymize_text(
            r#"failed C:\Users\Alice\Music\Session\src\take_one.wav and /home/bob/src/mix.flac"#,
        );
        assert!(text.contains("<path:take_one.wav>"), "{text}");
        assert!(text.contains("<path:mix.flac>"), "{text}");
        assert!(!text.contains("Alice"));
        assert!(!text.contains("bob"));
    }

    #[test]
    fn suppression_guard_is_scoped_and_nests() {
        assert!(!panic_reports_suppressed());
        {
            let _outer = suppress_panic_reports();
            assert!(panic_reports_suppressed());
            {
                let _inner = suppress_panic_reports();
                assert!(panic_reports_suppressed());
            }
            assert!(
                panic_reports_suppressed(),
                "the inner guard must not end the outer scope"
            );
        }
        assert!(!panic_reports_suppressed());
    }

    #[test]
    fn suppression_clears_after_a_caught_panic() {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(|| {
            let _guard = suppress_panic_reports();
            assert!(panic_reports_suppressed());
            panic!("simulated");
        });
        std::panic::set_hook(hook);
        assert!(result.is_err());
        assert!(
            !panic_reports_suppressed(),
            "unwinding must drop the guard and restore reporting"
        );
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
