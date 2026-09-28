//! Keeps sample rates from being written as bare numbers in production code.
//!
//! A `48_000` in the middle of a function is a guess about which rate is
//! meant -- the file's, the device's, a standard's -- and it goes stale the
//! moment any of those is something else. Rates belong in named constants
//! (`src/sample_rate.rs`, or a `const` beside the code a standard fixes them
//! for) or come from the file, buffer or device at hand. See the table at the
//! top of `src/sample_rate.rs`.
//!
//! Only unmistakable rates are checked. 8 000, 16 000 and 32 000 are left out:
//! as often as not they are a count or a frequency in Hz, and a rule that
//! cries wolf gets switched off.

use std::path::{Path, PathBuf};

const RATES: [&str; 8] = [
    "11025", "22050", "44100", "48000", "88200", "96000", "176400", "192000",
];

/// Files where a literal rate is the point: the vocabulary itself, CLI help
/// text, a video fixture writer and a debug generator.
const ALLOWED_FILES: [&str; 4] = [
    "src/sample_rate.rs",
    "src/cli.rs",
    "src/video/test_fixture.rs",
    "src/bin/debug_generate_long_mp3.rs",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Digits of every integer or float literal on the line, underscores removed.
fn numbers(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_ident = false;
    for c in code.chars() {
        if c.is_ascii_digit() || (c == '_' && !cur.is_empty()) {
            if cur.is_empty() && prev_ident {
                continue; // part of an identifier such as `f32` or `x48000`
            }
            if c != '_' {
                cur.push(c);
            }
        } else {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_ident = c.is_alphanumeric() || c == '_';
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Lines of `src` that are production code: not in a `#[cfg(test)]` or
/// `#[cfg(any())]` item, not a comment, not a `const` definition.
fn offending_lines(text: &str) -> Vec<(usize, String)> {
    let mut hits = Vec::new();
    let mut depth: i64 = 0;
    let mut skip_depth: Option<i64> = None;
    let mut pending_skip = false;
    let mut in_const = false;
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        let code = line.split("//").next().unwrap_or("");
        if trimmed.starts_with("#[cfg(test)]") || trimmed.starts_with("#[cfg(any())]") {
            pending_skip = true;
        } else if pending_skip && !trimmed.is_empty() && !trimmed.starts_with('#') {
            pending_skip = false;
            if code.contains('{') {
                skip_depth.get_or_insert(depth);
            }
        }
        let starts_const = trimmed.starts_with("const ")
            || trimmed.starts_with("pub const ")
            || trimmed.starts_with("pub(crate) const ")
            || trimmed.starts_with("pub(super) const ");
        if starts_const {
            in_const = true;
        }
        if skip_depth.is_none() && !in_const && !pending_skip {
            if numbers(code).iter().any(|n| RATES.contains(&n.as_str())) {
                hits.push((idx + 1, trimmed.to_string()));
            }
        }
        // A `const` ends at the `;` that closes the statement, not the one
        // inside an array type such as `[u32; 9]`.
        if in_const && code.trim_end().ends_with(';') {
            in_const = false;
        }
        depth += code.matches('{').count() as i64 - code.matches('}').count() as i64;
        if skip_depth.is_some_and(|d| depth <= d) {
            skip_depth = None;
        }
    }
    hits
}

#[test]
fn production_code_names_its_sample_rates() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    let mut report = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if ALLOWED_FILES.contains(&rel.as_str()) {
            continue;
        }
        let text = std::fs::read_to_string(&file).expect("read source");
        for (line, code) in offending_lines(&text) {
            report.push(format!("{rel}:{line}: {code}"));
        }
    }
    assert!(
        report.is_empty(),
        "sample rates written as bare numbers (use a named rate; see src/sample_rate.rs):\n{}",
        report.join("\n")
    );
}

#[test]
fn the_checker_sees_what_it_should() {
    let src = "fn f() {\n    let sr = 48_000;\n    let n = f32::MAX;\n}\nconst X: u32 = 44_100;\n#[cfg(test)]\nmod tests {\n    fn g() { let sr = 96000; }\n}\nfn h() { let a = 8000; }\n";
    let hits = offending_lines(src);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].0, 2);
}

#[test]
fn a_multi_line_const_table_is_one_definition() {
    let src = "const RATES: [u32; 3] = [\n    44_100, 48_000,\n    96_000,\n];\nfn f() { let sr = 48_000; }\n";
    let hits = offending_lines(src);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].0, 5);
}
