//! One way to ask "does this text match what the user typed".
//!
//! The list search, its highlight, the metadata-field search, the CLI's
//! `--query` and the column filters' text conditions all used to build their
//! own case-insensitive regex (or lowercase-and-`contains`) and fall back on
//! their own when the pattern did not compile. They agree now because they
//! are this.

use regex::{Regex, RegexBuilder};

#[derive(Clone, Debug)]
pub enum TextMatcher {
    /// Case-insensitive substring; holds the lowercased needle.
    Substring(String),
    /// Case-insensitive regular expression.
    Regex(Regex),
}

impl TextMatcher {
    /// The search box: a regex when regex mode is on and the pattern
    /// compiles, otherwise a plain substring -- a half-typed pattern such as
    /// `(ab` keeps matching literally instead of matching nothing.
    /// `None` for an empty query (nothing to filter by).
    pub fn search(query: &str, use_regex: bool) -> Option<Self> {
        let query = query.trim();
        if query.is_empty() {
            return None;
        }
        if use_regex {
            if let Some(re) = case_insensitive(query) {
                return Some(Self::Regex(re));
            }
        }
        Some(Self::Substring(query.to_lowercase()))
    }

    /// A regex the user asked for explicitly (a filter's "matches regex"
    /// condition). Unlike [`search`](Self::search) an invalid pattern is an
    /// error to show, not something to quietly reinterpret.
    pub fn regex(pattern: &str) -> Result<Self, regex::Error> {
        RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .map(Self::Regex)
    }

    /// An Excel-style pattern: `*` is any run of characters, `?` any one
    /// character, `~*` / `~?` / `~~` the literal characters. Anchored at both
    /// ends, so `kick*` means "begins with kick".
    pub fn wildcard(pattern: &str) -> Self {
        let mut re = String::from("^");
        let mut chars = pattern.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '~' if matches!(chars.peek(), Some('*' | '?' | '~')) => {
                    let lit = chars.next().unwrap_or('~');
                    re.push_str(&regex::escape(&lit.to_string()));
                }
                '*' => re.push_str(".*"),
                '?' => re.push('.'),
                other => re.push_str(&regex::escape(&other.to_string())),
            }
        }
        re.push('$');
        // Built from escaped pieces, so it always compiles.
        Self::Regex(case_insensitive(&re).expect("escaped wildcard pattern compiles"))
    }

    /// Whether `pattern` uses any Excel wildcard at all.
    pub fn has_wildcards(pattern: &str) -> bool {
        pattern.contains(['*', '?'])
    }

    pub fn is_match(&self, text: &str) -> bool {
        match self {
            Self::Substring(needle) => contains_ignore_case(text, needle),
            Self::Regex(re) => re.is_match(text),
        }
    }

    /// The regex that paints matches in the list: the pattern itself, or the
    /// escaped substring.
    pub fn highlight_regex(&self) -> Option<Regex> {
        match self {
            Self::Regex(re) => Some(re.clone()),
            Self::Substring(needle) => case_insensitive(&regex::escape(needle)),
        }
    }
}

fn case_insensitive(pattern: &str) -> Option<Regex> {
    RegexBuilder::new(pattern).case_insensitive(true).build().ok()
}

/// `needle` must already be lowercase. ASCII text -- nearly every file name --
/// is compared without allocating.
pub fn contains_ignore_case(text: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if text.is_ascii() && needle.is_ascii() {
        let (hay, pin) = (text.as_bytes(), needle.as_bytes());
        return hay.len() >= pin.len()
            && hay
                .windows(pin.len())
                .any(|window| window.eq_ignore_ascii_case(pin));
    }
    text.to_lowercase().contains(needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substring_is_case_insensitive() {
        let m = TextMatcher::search("Kick", false).unwrap();
        assert!(m.is_match("big_KICK_01.wav"));
        assert!(!m.is_match("snare.wav"));
        // Non-ASCII takes the allocating path and still folds case.
        let m = TextMatcher::search("ÄRGER", false).unwrap();
        assert!(m.is_match("großer ärger"));
    }

    #[test]
    fn empty_query_matches_nothing_to_filter() {
        assert!(TextMatcher::search("   ", true).is_none());
    }

    #[test]
    fn regex_mode_uses_the_pattern() {
        let m = TextMatcher::search(r"\d+bpm", true).unwrap();
        assert!(m.is_match("retake at 120BPM"));
        assert!(!m.is_match("no tempo"));
    }

    #[test]
    fn an_invalid_search_regex_falls_back_to_a_literal() {
        let m = TextMatcher::search("(ab", true).unwrap();
        assert!(matches!(m, TextMatcher::Substring(_)));
        assert!(m.is_match("x(abc"));
    }

    #[test]
    fn an_explicit_regex_reports_errors() {
        assert!(TextMatcher::regex("(ab").is_err());
        assert!(TextMatcher::regex("^ab").unwrap().is_match("ABc"));
    }

    #[test]
    fn wildcards_follow_excel() {
        let m = TextMatcher::wildcard("kick*");
        assert!(m.is_match("Kick_01"));
        assert!(!m.is_match("big kick"));
        let m = TextMatcher::wildcard("take_??");
        assert!(m.is_match("take_01"));
        assert!(!m.is_match("take_1"));
        // `~` escapes a wildcard; other regex metacharacters are literal.
        let m = TextMatcher::wildcard("what~?.wav");
        assert!(m.is_match("what?.wav"));
        assert!(!m.is_match("whatX.wav"));
        assert!(!m.is_match("what?xwav"));
    }

    #[test]
    fn highlight_matches_what_the_search_matches() {
        let m = TextMatcher::search("a.b", false).unwrap();
        let re = m.highlight_regex().unwrap();
        assert!(re.is_match("xA.Bx"));
        assert!(!re.is_match("aXb"), "a substring search is literal");
    }
}
