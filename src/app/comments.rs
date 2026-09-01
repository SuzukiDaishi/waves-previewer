//! The rules a shared session's conversation obeys, with no UI and no disk.
//!
//! A `.nwsess` on a file server has more than one writer and no lock, so a
//! comment layer only works if two people posting at the same moment produce
//! a result neither of them loses. Three decisions carry that:
//!
//! 1. **Ids are random, never counters.** `max(id) + 1` is what
//!    [`EditorNote`](crate::app::types::EditorNote) does, and it is exactly
//!    the collision the status/tag slugs and the content-addressed sidecars
//!    were introduced to remove.
//! 2. **The list is flat and merged as a set union.** Nesting would make two
//!    appends fight over the same array position; a union keyed by id is
//!    commutative and idempotent, so it can be applied in any order, twice,
//!    and still agree.
//! 3. **References live inside the body text.** A parallel array of targets
//!    would be a second copy of something the text already states, and the
//!    two would drift the first time somebody edited one of them.
//!
//! Times here are **seconds against the source file**, not sample indices.
//! `EditorNote` anchors by sample because it points into one person's editor
//! buffer, which shifts under destructive edits and sample-rate conversion
//! (hence `remap_editor_notes_for_replacement`). A comment points at the file
//! a colleague on another machine will open, so seconds are both the stable
//! choice and the readable one.

use std::collections::{HashMap, HashSet};

use super::project::ProjectComment;

/// Who the local machine posts as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentAuthor {
    /// The OS account name, trimmed and lowercased. Identity is decided by
    /// this alone, so a colleague renaming their `display_name` never orphans
    /// the comments they already wrote.
    pub id: String,
    /// The machine name, when the environment offers one.
    pub host: Option<String>,
    /// The `display_name=` pref. Display only.
    pub name: Option<String>,
}

impl CommentAuthor {
    /// The local identity, with `display_name` from prefs when it is set.
    pub fn local(display_name: Option<&str>) -> Self {
        let (user, host) = super::session_sync::local_user_and_host();
        let id = user
            .as_deref()
            .map(normalize_author_id)
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let name = display_name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        Self { id, host, name }
    }

    /// What to show for this author. Falls back to the account name, which is
    /// always present.
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

/// An account name compares case-insensitively: Windows hands back `Daishi`
/// where a shell hands back `daishi`, and they are the same person.
pub fn normalize_author_id(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// A fresh comment id: 128 random bits as hex, from the same source the
/// session lineage ids use.
pub fn new_comment_id() -> String {
    super::session_sync::new_session_id()
}

/// Whether `candidate` should replace `current` when both carry the same id.
///
/// Two versions of one comment only exist when its author edited it from two
/// processes at once, so the tie-breaks matter less than being *decided*:
/// every machine must pick the same winner, or the two keep overwriting each
/// other forever. `rev` first, then the edit stamp, then the body as a last
/// deterministic resort.
fn supersedes(candidate: &ProjectComment, current: &ProjectComment) -> bool {
    match candidate.rev.cmp(&current.rev) {
        std::cmp::Ordering::Greater => return true,
        std::cmp::Ordering::Less => return false,
        std::cmp::Ordering::Equal => {}
    }
    // A tombstone at the same revision wins: "this was withdrawn" is the
    // safer of the two answers to show.
    if candidate.deleted != current.deleted {
        return candidate.deleted;
    }
    let candidate_stamp = candidate.edited_at.as_deref().unwrap_or("");
    let current_stamp = current.edited_at.as_deref().unwrap_or("");
    match candidate_stamp.cmp(current_stamp) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => candidate.body > current.body,
    }
}

/// Fold `incoming` into `into`, keyed by id. Returns how many entries were
/// added or replaced, so a caller can tell whether anything actually moved.
///
/// Commutative and idempotent by construction: merging A into B and B into A
/// reach the same set, and merging twice changes nothing the second time.
pub fn merge_into(
    into: &mut Vec<ProjectComment>,
    incoming: impl IntoIterator<Item = ProjectComment>,
) -> usize {
    let mut index: HashMap<String, usize> = into
        .iter()
        .enumerate()
        .map(|(idx, comment)| (comment.id.clone(), idx))
        .collect();
    let mut changed = 0usize;
    for comment in incoming {
        match index.get(&comment.id) {
            Some(&idx) => {
                if supersedes(&comment, &into[idx]) {
                    into[idx] = comment;
                    changed += 1;
                }
            }
            None => {
                index.insert(comment.id.clone(), into.len());
                into.push(comment);
                changed += 1;
            }
        }
    }
    if changed > 0 {
        sort_for_storage(into);
    }
    changed
}

/// A stable on-disk order, so two people writing the same set of comments
/// produce the same bytes and the document stops churning in diffs.
pub fn sort_for_storage(comments: &mut [ProjectComment]) {
    comments.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// One node of the rendered thread: a comment plus the replies under it.
#[derive(Clone, Debug, PartialEq)]
pub struct CommentNode {
    pub comment: ProjectComment,
    pub replies: Vec<CommentNode>,
}

impl CommentNode {
    /// Every comment in this subtree, including the root.
    pub fn len(&self) -> usize {
        1 + self.replies.iter().map(CommentNode::len).sum::<usize>()
    }
}

/// Build the reply forest.
///
/// Two shapes have to survive, because a shared document can hold both: a
/// reply whose parent is not here (the author's copy was never written, or an
/// older build dropped it) and a parent chain that loops (nothing produces
/// one today, but a hand-edited document could, and an infinite walk in the
/// UI thread is a hung window). Both are promoted to roots rather than
/// dropped -- a comment somebody wrote is not ours to discard.
pub fn build_threads(comments: &[ProjectComment]) -> Vec<CommentNode> {
    let known: HashSet<&str> = comments.iter().map(|c| c.id.as_str()).collect();
    let parent_of: HashMap<&str, &str> = comments
        .iter()
        .filter_map(|c| c.parent.as_deref().map(|parent| (c.id.as_str(), parent)))
        .collect();

    // A node is a root when it names no parent, names one we do not have, or
    // sits on a cycle.
    let is_root = |comment: &ProjectComment| -> bool {
        let Some(parent) = comment.parent.as_deref() else {
            return true;
        };
        if !known.contains(parent) {
            return true;
        }
        let mut seen: HashSet<&str> = HashSet::from([comment.id.as_str()]);
        let mut cursor = parent;
        while let Some(&next) = parent_of.get(cursor) {
            if !seen.insert(cursor) {
                return true;
            }
            if !known.contains(next) {
                return false;
            }
            cursor = next;
        }
        !seen.insert(cursor)
    };

    let mut children: HashMap<&str, Vec<&ProjectComment>> = HashMap::new();
    let mut roots: Vec<&ProjectComment> = Vec::new();
    for comment in comments {
        if is_root(comment) {
            roots.push(comment);
        } else {
            let parent = comment.parent.as_deref().unwrap_or_default();
            children.entry(parent).or_default().push(comment);
        }
    }

    let order = |a: &&ProjectComment, b: &&ProjectComment| {
        a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id))
    };
    roots.sort_by(order);
    for bucket in children.values_mut() {
        bucket.sort_by(order);
    }

    fn assemble(
        comment: &ProjectComment,
        children: &HashMap<&str, Vec<&ProjectComment>>,
    ) -> CommentNode {
        let replies = children
            .get(comment.id.as_str())
            .map(|bucket| {
                bucket
                    .iter()
                    .map(|child| assemble(child, children))
                    .collect()
            })
            .unwrap_or_default();
        CommentNode {
            comment: comment.clone(),
            replies,
        }
    }

    roots
        .into_iter()
        .map(|root| assemble(root, &children))
        .collect()
}

/// Where on a file's timeline a reference points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CommentAnchor {
    pub start_sec: f64,
    /// `None` for a point in time rather than a span.
    pub end_sec: Option<f64>,
    /// A spectral band, for a reference authored over a spectrogram selection.
    pub freq_hz: Option<(f32, f32)>,
}

impl CommentAnchor {
    /// The span, ordered, or `None` when this is a single point.
    pub fn normalized_range(&self) -> Option<(f64, f64)> {
        let end = self.end_sec?;
        (end != self.start_sec).then(|| (self.start_sec.min(end), self.start_sec.max(end)))
    }
}

/// A file, optionally with a position on its timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct CommentRef {
    /// Written under the session's own `path_mode`, exactly like every other
    /// stored source path, so it resolves through the same repair chain.
    pub path: String,
    pub anchor: Option<CommentAnchor>,
}

const REF_OPEN: &str = "@[";

/// Render a reference as the token that goes in a comment body.
pub fn format_ref(reference: &CommentRef) -> String {
    let mut out = String::from(REF_OPEN);
    out.push_str(&escape_path(&reference.path));
    if let Some(anchor) = reference.anchor {
        out.push('|');
        match anchor.normalized_range() {
            Some((start, end)) => {
                out.push_str(&format_seconds(start));
                out.push('-');
                out.push_str(&format_seconds(end));
            }
            None => out.push_str(&format_seconds(anchor.start_sec)),
        }
        if let Some((low, high)) = anchor.freq_hz {
            let (low, high) = (low.min(high), low.max(high));
            out.push('|');
            out.push_str(&format!("{low:.0}-{high:.0}Hz"));
        }
    }
    out.push(']');
    out
}

/// Three decimals is a millisecond, which is finer than anyone points at a
/// waveform by hand and short enough to stay readable in the raw TOML.
fn format_seconds(value: f64) -> String {
    let text = format!("{value:.3}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

fn escape_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        if matches!(ch, '\\' | ']' | '|') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Every reference token in `body`, as byte ranges into it.
///
/// Anything that does not parse is simply not returned, so a stray `@[` a
/// person typed stays the text they typed rather than becoming a broken link.
pub fn find_refs(body: &str) -> Vec<(std::ops::Range<usize>, CommentRef)> {
    let bytes = body.as_bytes();
    let mut found = Vec::new();
    let mut cursor = 0usize;
    while let Some(offset) = body[cursor..].find(REF_OPEN) {
        let start = cursor + offset;
        let inner_start = start + REF_OPEN.len();
        // Find the unescaped `]` that closes it.
        let mut idx = inner_start;
        let mut close = None;
        while idx < bytes.len() {
            match bytes[idx] {
                b'\\' => idx += 2,
                b']' => {
                    close = Some(idx);
                    break;
                }
                _ => idx += 1,
            }
        }
        let Some(close) = close else { break };
        match parse_ref_body(&body[inner_start..close]) {
            Some(reference) => {
                found.push((start..close + 1, reference));
                cursor = close + 1;
            }
            // Not a reference after all. Step past the `@` only, so an
            // `@[` nested inside the rejected text still gets its chance.
            None => cursor = start + 1,
        }
    }
    found
}

fn parse_ref_body(inner: &str) -> Option<CommentRef> {
    let mut fields: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => current.push(chars.next()?),
            '|' => {
                fields.push(std::mem::take(&mut current));
                // Only the path is escaped; the numeric fields never contain
                // a separator, so splitting the rest plainly is enough.
                let rest: String = chars.by_ref().collect();
                for field in rest.split('|') {
                    fields.push(field.to_string());
                }
                current = String::new();
                break;
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() || fields.is_empty() {
        fields.push(current);
    }

    let path = fields.first()?.trim().to_string();
    if path.is_empty() {
        return None;
    }
    let anchor = match fields.len() {
        1 => None,
        2 | 3 => {
            let (start_sec, end_sec) = parse_time_field(fields[1].trim())?;
            let freq_hz = match fields.get(2) {
                Some(field) => Some(parse_freq_field(field.trim())?),
                None => None,
            };
            Some(CommentAnchor {
                start_sec,
                end_sec,
                freq_hz,
            })
        }
        _ => return None,
    };
    Some(CommentRef { path, anchor })
}

fn parse_time_field(field: &str) -> Option<(f64, Option<f64>)> {
    match field.split_once('-') {
        Some((start, end)) => {
            let start = parse_seconds(start)?;
            let end = parse_seconds(end)?;
            Some((start, Some(end)))
        }
        None => Some((parse_seconds(field)?, None)),
    }
}

fn parse_seconds(text: &str) -> Option<f64> {
    let value: f64 = text.trim().parse().ok()?;
    (value.is_finite() && value >= 0.0).then_some(value)
}

fn parse_freq_field(field: &str) -> Option<(f32, f32)> {
    let body = field.strip_suffix("Hz").or_else(|| field.strip_suffix("hz"))?;
    let (low, high) = body.split_once('-')?;
    let low: f32 = low.trim().parse().ok()?;
    let high: f32 = high.trim().parse().ok()?;
    (low.is_finite() && high.is_finite() && low >= 0.0 && high >= 0.0)
        .then_some((low.min(high), low.max(high)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(id: &str, parent: Option<&str>, created_at: &str) -> ProjectComment {
        ProjectComment {
            id: id.to_string(),
            parent: parent.map(str::to_string),
            author_id: "daishi".to_string(),
            author_host: Some("WS-01".to_string()),
            author_name: None,
            created_at: created_at.to_string(),
            edited_at: None,
            rev: 0,
            body: format!("body {id}"),
            deleted: false,
            resolved_by: None,
            resolved_at: None,
        }
    }

    fn ids(nodes: &[CommentNode]) -> Vec<String> {
        nodes.iter().map(|n| n.comment.id.clone()).collect()
    }

    #[test]
    fn two_people_posting_at_once_both_survive() {
        let mut mine = vec![comment("a", None, "2026-09-01T00:00:00Z")];
        let theirs = vec![comment("b", None, "2026-09-01T00:00:01Z")];
        assert_eq!(merge_into(&mut mine, theirs.clone()), 1);
        assert_eq!(mine.len(), 2);

        // The other direction reaches the same set: the merge is commutative.
        let mut reversed = theirs;
        merge_into(&mut reversed, vec![comment("a", None, "2026-09-01T00:00:00Z")]);
        assert_eq!(
            mine.iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
            reversed.iter().map(|c| c.id.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn merging_the_same_comments_twice_changes_nothing() {
        let incoming = vec![comment("a", None, "2026-09-01T00:00:00Z")];
        let mut into = incoming.clone();
        assert_eq!(merge_into(&mut into, incoming), 0);
        assert_eq!(into.len(), 1);
    }

    #[test]
    fn the_higher_revision_wins_an_edit_race() {
        let mut into = vec![comment("a", None, "2026-09-01T00:00:00Z")];
        let mut newer = comment("a", None, "2026-09-01T00:00:00Z");
        newer.rev = 1;
        newer.edited_at = Some("2026-09-01T00:05:00Z".to_string());
        newer.body = "edited".to_string();
        assert_eq!(merge_into(&mut into, vec![newer]), 1);
        assert_eq!(into[0].body, "edited");

        // ...and the older one cannot win it back.
        let mut older = comment("a", None, "2026-09-01T00:00:00Z");
        older.body = "stale".to_string();
        assert_eq!(merge_into(&mut into, vec![older]), 0);
        assert_eq!(into[0].body, "edited");
    }

    #[test]
    fn a_tombstone_beats_a_body_at_the_same_revision() {
        let mut into = vec![comment("a", None, "2026-09-01T00:00:00Z")];
        let mut removed = comment("a", None, "2026-09-01T00:00:00Z");
        removed.deleted = true;
        assert_eq!(merge_into(&mut into, vec![removed]), 1);
        assert!(into[0].deleted);
    }

    #[test]
    fn replies_hang_under_their_parent_in_time_order() {
        let comments = vec![
            comment("root", None, "2026-09-01T00:00:00Z"),
            comment("second", Some("root"), "2026-09-01T00:00:02Z"),
            comment("first", Some("root"), "2026-09-01T00:00:01Z"),
            comment("nested", Some("first"), "2026-09-01T00:00:03Z"),
        ];
        let threads = build_threads(&comments);
        assert_eq!(ids(&threads), vec!["root"]);
        assert_eq!(ids(&threads[0].replies), vec!["first", "second"]);
        assert_eq!(ids(&threads[0].replies[0].replies), vec!["nested"]);
        assert_eq!(threads[0].len(), 4);
    }

    #[test]
    fn a_reply_whose_parent_is_missing_is_promoted_not_dropped() {
        let comments = vec![comment("orphan", Some("gone"), "2026-09-01T00:00:00Z")];
        let threads = build_threads(&comments);
        assert_eq!(ids(&threads), vec!["orphan"]);
    }

    #[test]
    fn a_parent_cycle_terminates_instead_of_hanging_the_ui() {
        let comments = vec![
            comment("a", Some("b"), "2026-09-01T00:00:00Z"),
            comment("b", Some("a"), "2026-09-01T00:00:01Z"),
        ];
        let threads = build_threads(&comments);
        // Both are reachable; neither walk ran forever.
        let total: usize = threads.iter().map(CommentNode::len).sum();
        assert_eq!(total, 2);
    }

    #[test]
    fn reference_tokens_round_trip() {
        let cases = vec![
            CommentRef {
                path: "voice/line_001.wav".to_string(),
                anchor: None,
            },
            CommentRef {
                path: "voice/line_001.wav".to_string(),
                anchor: Some(CommentAnchor {
                    start_sec: 12.5,
                    end_sec: None,
                    freq_hz: None,
                }),
            },
            CommentRef {
                path: "voice/line_001.wav".to_string(),
                anchor: Some(CommentAnchor {
                    start_sec: 12.5,
                    end_sec: Some(14.25),
                    freq_hz: None,
                }),
            },
            CommentRef {
                path: "voice/line_001.wav".to_string(),
                anchor: Some(CommentAnchor {
                    start_sec: 12.5,
                    end_sec: Some(14.25),
                    freq_hz: Some((220.0, 880.0)),
                }),
            },
        ];
        for reference in cases {
            let token = format_ref(&reference);
            let found = find_refs(&token);
            assert_eq!(found.len(), 1, "{token}");
            assert_eq!(found[0].0, 0..token.len());
            assert_eq!(found[0].1, reference, "{token}");
        }
    }

    #[test]
    fn a_path_carrying_the_separators_survives_escaping() {
        let reference = CommentRef {
            path: r"odd]name|with\slashes.wav".to_string(),
            anchor: None,
        };
        let token = format_ref(&reference);
        let found = find_refs(&token);
        assert_eq!(found.len(), 1, "{token}");
        assert_eq!(found[0].1, reference);
    }

    #[test]
    fn text_around_a_reference_is_reported_by_range() {
        let body = "check @[a.wav|1.5] and @[b.wav] please";
        let found = find_refs(body);
        assert_eq!(found.len(), 2);
        assert_eq!(&body[found[0].0.clone()], "@[a.wav|1.5]");
        assert_eq!(&body[found[1].0.clone()], "@[b.wav]");
        assert_eq!(found[1].1.path, "b.wav");
    }

    #[test]
    fn malformed_tokens_stay_plain_text() {
        for body in [
            "@[",
            "@[]",
            "@[a.wav|]",
            "@[a.wav|abc]",
            "@[a.wav|1.0|900]",
            "@[a.wav|-1.0]",
            "@[a.wav|1.0|1.0-2.0|3.0-4.0Hz]",
        ] {
            assert!(find_refs(body).is_empty(), "{body} should not parse");
        }
    }

    #[test]
    fn an_account_name_compares_case_insensitively() {
        assert_eq!(normalize_author_id("  Daishi "), "daishi");
    }

    #[test]
    fn two_ids_never_collide() {
        let a = new_comment_id();
        let b = new_comment_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
    }
}
