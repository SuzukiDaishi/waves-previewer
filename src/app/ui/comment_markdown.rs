//! A deliberately small Markdown, parsed here and painted in `ui/comments.rs`.
//!
//! Small because the whole of CommonMark is not what people write in a note
//! about a sound. Bold, a bullet list, a code span for a filename, a quote of
//! what somebody else said -- past that the syntax costs more attention than
//! it saves. Keeping it small also keeps it dependency-free, which matters in
//! a repository that regenerates a licence snapshot for every crate it adds.
//!
//! The parser is pure and lives apart from the drawing for the usual reason:
//! this is the part that is easy to get wrong on an input nobody anticipated,
//! and it is only testable while it has no `Ui` in its signature.
//!
//! Where it differs from CommonMark it differs by being simpler, never by
//! being clever. Emphasis nests (`**bold *and italic* here**`) but the
//! `***both***` shorthand is not special -- CommonMark needs a run-length
//! rule for it, and a marker that turns out not to close simply stays the
//! character somebody typed, which is the behavior a person writing a note
//! about `*.wav` actually wants.

use crate::app::comments::{self, CommentRef};

/// Which of the inline markers are in force over a run of text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpanStyle {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
}

/// One run inside a block.
#[derive(Clone, Debug, PartialEq)]
pub enum Span {
    Text { text: String, style: SpanStyle },
    /// A bare URL somebody typed. Not a Markdown link -- `[a](b)` is more
    /// syntax than this is worth, and a pasted address is what people
    /// actually write.
    Link(String),
    /// `@[path|time]`, drawn as a chip you can press.
    Reference(CommentRef),
}

/// One paragraph-level piece of a comment.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading { level: u8, spans: Vec<Span> },
    Paragraph(Vec<Span>),
    /// `ordinal` is `None` for a bullet and `Some(n)` for a numbered item.
    Item { ordinal: Option<u32>, spans: Vec<Span> },
    Quote(Vec<Span>),
    /// A fenced block, kept exactly as written.
    Code(String),
}

/// How deep emphasis may nest before the parser stops looking.
///
/// Not a correctness limit -- it is what keeps a body of nothing but
/// asterisks from recursing as far as the text is long on the UI thread.
const MAX_NESTING: usize = 4;

/// Split a comment body into blocks.
pub fn parse_comment_body(body: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    let mut fence: Option<Vec<String>> = None;

    let flush = |paragraph: &mut Vec<&str>, blocks: &mut Vec<Block>| {
        if paragraph.is_empty() {
            return;
        }
        let joined = paragraph.join("\n");
        paragraph.clear();
        blocks.push(Block::Paragraph(parse_spans(&joined)));
    };

    for line in body.lines() {
        if let Some(lines) = fence.as_mut() {
            if line.trim_start().starts_with("```") {
                blocks.push(Block::Code(lines.join("\n")));
                fence = None;
            } else {
                lines.push(line.to_string());
            }
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            flush(&mut paragraph, &mut blocks);
            fence = Some(Vec::new());
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut paragraph, &mut blocks);
            continue;
        }
        if let Some(block) = parse_block_line(trimmed) {
            flush(&mut paragraph, &mut blocks);
            blocks.push(block);
            continue;
        }
        paragraph.push(line);
    }
    // An unterminated fence is still a fence: the alternative is silently
    // reading the rest of the note as prose the author did not write.
    if let Some(lines) = fence {
        blocks.push(Block::Code(lines.join("\n")));
    }
    flush(&mut paragraph, &mut blocks);
    blocks
}

fn parse_block_line(trimmed: &str) -> Option<Block> {
    for level in (1u8..=3).rev() {
        let marker = format!("{} ", "#".repeat(level as usize));
        if let Some(rest) = trimmed.strip_prefix(&marker) {
            return Some(Block::Heading {
                level,
                spans: parse_spans(rest),
            });
        }
    }
    if let Some(rest) = trimmed.strip_prefix("> ").or(trimmed.strip_prefix(">")) {
        return Some(Block::Quote(parse_spans(rest.trim_start())));
    }
    for marker in ["- ", "* "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return Some(Block::Item {
                ordinal: None,
                spans: parse_spans(rest),
            });
        }
    }
    if let Some((digits, rest)) = split_ordinal(trimmed) {
        return Some(Block::Item {
            ordinal: Some(digits),
            spans: parse_spans(rest),
        });
    }
    None
}

fn split_ordinal(text: &str) -> Option<(u32, &str)> {
    let end = text.find(". ")?;
    let digits: u32 = text[..end].parse().ok()?;
    Some((digits, &text[end + 2..]))
}

/// Split a run of text into styled spans, references and links.
pub fn parse_spans(text: &str) -> Vec<Span> {
    let mut out = Vec::new();
    parse_styled(text, SpanStyle::default(), 0, &mut out);
    out
}

fn parse_styled(text: &str, style: SpanStyle, depth: usize, out: &mut Vec<Span>) {
    let mut plain = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        // A code span swallows its contents whole: the point of writing
        // `*.wav` in backticks is that the asterisk stays an asterisk.
        if let Some(inner) = rest.strip_prefix('`') {
            if let Some(end) = inner.find('`') {
                push_text(&mut plain, style, out);
                out.push(Span::Text {
                    text: inner[..end].to_string(),
                    style: SpanStyle { code: true, ..style },
                });
                rest = &inner[end + 1..];
                continue;
            }
        }
        if rest.starts_with("@[") {
            let found = comments::find_refs(rest);
            if let Some((range, reference)) = found.into_iter().next() {
                if range.start == 0 {
                    push_text(&mut plain, style, out);
                    out.push(Span::Reference(reference));
                    rest = &rest[range.end..];
                    continue;
                }
            }
        }
        if let Some(url_end) = url_length(rest) {
            push_text(&mut plain, style, out);
            out.push(Span::Link(rest[..url_end].to_string()));
            rest = &rest[url_end..];
            continue;
        }
        if depth < MAX_NESTING {
            if let Some((marker, next_style)) = emphasis_at(rest, style) {
                if let Some(end) = rest[marker.len()..].find(marker) {
                    let inner = &rest[marker.len()..marker.len() + end];
                    if !inner.is_empty() {
                        push_text(&mut plain, style, out);
                        parse_styled(inner, next_style, depth + 1, out);
                        rest = &rest[marker.len() * 2 + end..];
                        continue;
                    }
                }
            }
        }
        let ch_len = rest.chars().next().map(char::len_utf8).unwrap_or(1);
        plain.push_str(&rest[..ch_len]);
        rest = &rest[ch_len..];
    }
    push_text(&mut plain, style, out);
}

/// The emphasis marker starting here, and the style inside it. `**` is
/// checked before `*` so bold is never read as two italics.
fn emphasis_at(rest: &str, style: SpanStyle) -> Option<(&'static str, SpanStyle)> {
    if rest.starts_with("**") && !style.bold {
        return Some(("**", SpanStyle { bold: true, ..style }));
    }
    if rest.starts_with("~~") && !style.strike {
        return Some((
            "~~",
            SpanStyle {
                strike: true,
                ..style
            },
        ));
    }
    if rest.starts_with('*') && !style.italic {
        return Some((
            "*",
            SpanStyle {
                italic: true,
                ..style
            },
        ));
    }
    None
}

/// How many bytes of `rest` are a bare URL, if it starts with one.
fn url_length(rest: &str) -> Option<usize> {
    if !rest.starts_with("http://") && !rest.starts_with("https://") {
        return None;
    }
    let end = rest
        .find(|ch: char| ch.is_whitespace())
        .unwrap_or(rest.len());
    // Sentence punctuation after a pasted address is not part of it.
    let trimmed = rest[..end].trim_end_matches(['.', ',', ')', ']', '。', '、']);
    (trimmed.len() > "https://".len()).then_some(trimmed.len())
}

fn push_text(plain: &mut String, style: SpanStyle, out: &mut Vec<Span>) {
    if plain.is_empty() {
        return;
    }
    out.push(Span::Text {
        text: std::mem::take(plain),
        style,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(text: &str) -> Vec<Span> {
        parse_spans(text)
    }

    fn plain(text: &str, style: SpanStyle) -> Span {
        Span::Text {
            text: text.to_string(),
            style,
        }
    }

    #[test]
    fn emphasis_nests_and_bold_is_never_two_italics() {
        assert_eq!(
            spans("a **b** c"),
            vec![
                plain("a ", SpanStyle::default()),
                plain(
                    "b",
                    SpanStyle {
                        bold: true,
                        ..Default::default()
                    }
                ),
                plain(" c", SpanStyle::default()),
            ]
        );
        assert_eq!(
            spans("**bold *and italic* here**"),
            vec![
                plain(
                    "bold ",
                    SpanStyle {
                        bold: true,
                        ..Default::default()
                    }
                ),
                plain(
                    "and italic",
                    SpanStyle {
                        bold: true,
                        italic: true,
                        ..Default::default()
                    }
                ),
                plain(
                    " here",
                    SpanStyle {
                        bold: true,
                        ..Default::default()
                    }
                ),
            ]
        );
    }

    #[test]
    fn the_commonmark_triple_marker_is_not_special_here() {
        // `***both***` needs CommonMark's run-length rule. Without it the
        // outer `**` closes on the first `**` it finds and the leftover `*`
        // stays a character, which is the documented behaviour -- and the
        // same rule that keeps `2 * 3` intact.
        let parsed = spans("***both***");
        assert!(parsed.iter().any(|span| matches!(
            span,
            Span::Text { style, .. } if style.bold
        )));
    }

    #[test]
    fn a_code_span_keeps_its_contents_literal() {
        assert_eq!(
            spans("rename `*.wav` first"),
            vec![
                plain("rename ", SpanStyle::default()),
                plain(
                    "*.wav",
                    SpanStyle {
                        code: true,
                        ..Default::default()
                    }
                ),
                plain(" first", SpanStyle::default()),
            ]
        );
    }

    #[test]
    fn an_unclosed_marker_stays_the_character_it_is() {
        assert_eq!(spans("2 * 3"), vec![plain("2 * 3", SpanStyle::default())]);
        assert_eq!(
            spans("a `b"),
            vec![plain("a `b", SpanStyle::default())]
        );
    }

    #[test]
    fn a_reference_survives_inside_a_sentence() {
        let parsed = spans("listen to @[a.wav|1.5] here");
        assert_eq!(parsed.len(), 3);
        assert!(matches!(parsed[1], Span::Reference(_)));
        let Span::Reference(reference) = &parsed[1] else {
            unreachable!()
        };
        assert_eq!(reference.path, "a.wav");
    }

    #[test]
    fn a_pasted_address_becomes_a_link_without_its_full_stop() {
        assert_eq!(
            spans("see https://example.com/x. thanks"),
            vec![
                plain("see ", SpanStyle::default()),
                Span::Link("https://example.com/x".to_string()),
                plain(". thanks", SpanStyle::default()),
            ]
        );
    }

    #[test]
    fn blocks_split_on_blank_lines_and_markers() {
        let blocks = parse_comment_body(
            "# Title\n\nfirst para\nstill first\n\n- one\n- two\n3. three\n\n> quoted\n",
        );
        assert!(matches!(blocks[0], Block::Heading { level: 1, .. }));
        assert!(matches!(blocks[1], Block::Paragraph(_)));
        assert!(matches!(
            blocks[2],
            Block::Item { ordinal: None, .. }
        ));
        assert!(matches!(
            blocks[4],
            Block::Item {
                ordinal: Some(3),
                ..
            }
        ));
        assert!(matches!(blocks[5], Block::Quote(_)));
    }

    #[test]
    fn a_fence_is_kept_exactly_and_survives_being_left_open() {
        let blocks = parse_comment_body("before\n```\n**not bold**\n```\nafter");
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[1], Block::Code("**not bold**".to_string()));

        let unterminated = parse_comment_body("```\nstill code");
        assert_eq!(unterminated, vec![Block::Code("still code".to_string())]);
    }

    #[test]
    fn a_body_of_nothing_but_markers_terminates() {
        // Not a correctness case, a liveness one: this runs on the UI thread.
        let parsed = parse_comment_body(&"*".repeat(200));
        assert!(!parsed.is_empty());
    }

    #[test]
    fn an_empty_body_parses_to_nothing() {
        assert!(parse_comment_body("").is_empty());
        assert!(parse_comment_body("\n\n").is_empty());
    }
}
