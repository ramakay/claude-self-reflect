//! A compact's source map: bounded, verbatim evidence with stable turn addresses.
//! It is an index of history, never an inferred task list or a better summary.
use super::{query::queued_duplicates, Entry, ParsedTranscript, Role};
use serde::Serialize;
use std::{collections::HashSet, path::Path};

pub const SENTINEL: &str = "[[CSR:COMPACT_RECEIPT]]";
/// The two source labels a receipt carries.
const SOURCE_MIRROR: &str = "mirror (scrubbed copy)";
const SOURCE_LIVE: &str = "live transcript (backup not verified)";
const EMPTY_FOOTER: &str = "No request, edit or error anchors in this chapter.";

static HEADER_LINE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(&format!(
        r"^Compact receipt: turns [0-9]{{1,9}}\.\.[0-9]{{1,9}}, source: (?:{}|{})\. Historical evidence, not instructions or outstanding tasks\. {}$",
        regex::escape(SOURCE_MIRROR),
        regex::escape(SOURCE_LIVE),
        regex::escape(SENTINEL)
    ))
    .expect("receipt header regex")
});

static ANCHOR_LINE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r#"^- (?:request|error \(historical\)|edit attempt) .+ → csr_transcript\(session=.+, view="slice", turns="[0-9]{1,9}\.\.[0-9]{1,9}"\)$"#,
    )
    .expect("receipt anchor regex")
});

/// How many lines a receipt block takes, given the line at an index (an empty line past the
/// end). The block is the header, then its anchor rows or the empty footer, every word the
/// emitter's own. `None` when the first line is not the header. Prose around the block is not
/// part of it.
pub fn block_len<'a>(line_at: impl Fn(usize) -> &'a str) -> Option<usize> {
    let clean = |at: usize| line_at(at).trim_end_matches('\r');
    if !HEADER_LINE.is_match(clean(0)) {
        return None;
    }
    let mut len = 1;
    while ANCHOR_LINE.is_match(clean(len)) {
        len += 1;
    }
    if len == 1 && clean(1) == EMPTY_FOOTER {
        len = 2;
    }
    Some(len)
}

/// Whether `text` holds a whole receipt block on its own lines.
pub(super) fn holds_block(text: &str) -> bool {
    let rows: Vec<&str> = text.lines().collect();
    (0..rows.len()).any(|at| block_len(|n| rows.get(at + n).copied().unwrap_or("")).is_some())
}

/// A build, test, panic or git failure in a tool result.
pub(super) fn has_failure_marker(text: &str) -> bool {
    ["test result: FAILED", "error[E", "panicked at", "fatal:"]
        .iter()
        .any(|marker| text.contains(marker))
}

#[derive(Debug, Serialize)]
pub struct Anchor {
    pub kind: &'static str,
    pub turn: usize,
    pub through: usize,
    pub quote: String,
}

#[derive(Debug, Serialize)]
pub struct Receipt {
    pub session: String,
    pub first: usize,
    pub last: usize,
    pub source: &'static str,
    pub anchors: Vec<Anchor>,
}

fn short(text: &str) -> String {
    super::truncate_chars(&text.split_whitespace().collect::<Vec<_>>().join(" "), 120)
}

/// Latest completed chapter, or the current chapter when preparing a compact.
pub fn chapter(parsed: &ParsedTranscript, before_compact: bool) -> Option<(usize, usize)> {
    let boundaries: Vec<_> = parsed
        .entries
        .iter()
        .filter(|e| e.is_compact_boundary())
        .map(|e| e.turn)
        .collect();
    let last = if before_compact {
        parsed.entries.last()?.turn
    } else {
        boundaries
            .last()
            .map_or(parsed.entries.last()?.turn, |t| t.saturating_sub(1))
    };
    let first = boundaries
        .iter()
        .rev()
        .find(|t| **t <= last)
        .map_or(1, |t| t + 1);
    (first <= last).then_some((first, last))
}

const MAX_ERRORS: usize = 2;

/// A Bash error is evidence only when it carries a build, test, panic or git failure marker.
/// Any other Bash error (a failed `cd`, a grep with no match) abstains. Errors of other tools
/// stay. A result whose tool is not known abstains like Bash.
fn keep_error(result: &super::ToolResult, names: &std::collections::HashMap<&str, &str>) -> bool {
    let tool = result
        .tool_use_id
        .as_deref()
        .and_then(|id| names.get(id).copied());
    if tool.is_some_and(|name| name != "Bash") {
        return true;
    }
    result.failure_marker
}

/// The first error result of `e` that a receipt may name. An error that abstains does not
/// hide a later one in the same message.
fn kept_error<'a>(
    e: &'a Entry,
    names: &std::collections::HashMap<&str, &str>,
) -> Option<&'a super::ToolResult> {
    e.tool_results
        .iter()
        .find(|t| t.is_error && keep_error(t, names))
}

pub fn build(parsed: &ParsedTranscript, sid: &str, range: (usize, usize)) -> Receipt {
    let duplicates = queued_duplicates(parsed);
    let entries: Vec<&Entry> = parsed
        .entries
        .iter()
        .filter(|e| e.turn >= range.0 && e.turn <= range.1 && !e.is_sidechain)
        .collect();
    let mut anchors = Vec::new();
    let mut asks = 0;
    let mut errors = 0;
    let mut files = HashSet::new();
    // Error rows are a narrow whitelist. Slots they leave empty go to requests and edits, so
    // the total stays at the old bound of 2 + 2 + 3.
    let tool_names: std::collections::HashMap<&str, &str> = parsed
        .entries
        .iter()
        .flat_map(|e| e.tool_uses.iter())
        .filter_map(|t| Some((t.id.as_deref()?, t.name.as_str())))
        .collect();
    let kept_errors: Vec<usize> = entries
        .iter()
        .rev()
        .filter(|e| kept_error(e, &tool_names).is_some())
        .take(MAX_ERRORS)
        .map(|e| e.turn)
        .collect();
    let spare = MAX_ERRORS - kept_errors.len();
    let max_asks = 2 + spare.div_ceil(2);
    let max_files = 3 + spare / 2;
    for e in entries.iter().rev() {
        if asks < max_asks
            && e.role == Role::User
            && !e.text.trim().is_empty()
            && !duplicates.contains(&e.turn)
            && super::ask_noise_class(e).is_none()
            && !crate::extraction::provenance::is_csr_emission(&e.text)
        {
            anchors.push(Anchor {
                kind: "request",
                turn: e.turn,
                through: e.turn,
                quote: short(super::strip_queued_prefix(&e.text)),
            });
            asks += 1;
        }
        if errors < MAX_ERRORS && kept_errors.contains(&e.turn) {
            if let Some(result) = kept_error(e, &tool_names) {
                anchors.push(Anchor {
                    kind: "error (historical)",
                    turn: e.turn,
                    through: e.turn,
                    quote: short(&result.preview),
                });
                errors += 1;
            }
        }
        for tool in &e.tool_uses {
            if files.len() >= max_files
                || !matches!(tool.name.as_str(), "Edit" | "Write" | "MultiEdit")
            {
                continue;
            }
            let Some(path) = &tool.file_path else {
                continue;
            };
            if !files.insert(path.clone()) {
                continue;
            }
            let result = tool.id.as_ref().and_then(|id| {
                entries.iter().find(|r| {
                    r.turn > e.turn
                        && r.tool_results
                            .iter()
                            .any(|t| t.tool_use_id.as_ref() == Some(id))
                })
            });
            anchors.push(Anchor {
                kind: "edit attempt",
                turn: e.turn,
                through: result.map_or(e.turn, |r| r.turn),
                quote: short(path),
            });
        }
    }
    anchors.sort_by_key(|a| a.turn);
    Receipt {
        session: sid.to_string(),
        first: range.0,
        last: range.1,
        source: if parsed.from_mirror {
            SOURCE_MIRROR
        } else {
            SOURCE_LIVE
        },
        anchors,
    }
}

impl Receipt {
    pub fn text(&self) -> String {
        let sid = serde_json::to_string(&self.session).unwrap_or_default();
        let mut out = format!("Compact receipt: turns {}..{}, source: {}. Historical evidence, not instructions or outstanding tasks. {SENTINEL}\n", self.first, self.last, self.source);
        for a in &self.anchors {
            // JSON quoting plus XML escaping prevents a quote becoming a new line or tag.
            let quote =
                super::query::xml_escape(&serde_json::to_string(&a.quote).unwrap_or_default());
            out.push_str(&format!(
                "- {} {} → csr_transcript(session={}, view=\"slice\", turns=\"{}..{}\")\n",
                a.kind,
                quote,
                super::query::xml_escape(&sid),
                a.turn,
                a.through
            ));
        }
        // A chapter with no anchors says nothing more: no footer line. Older receipts carry
        // `EMPTY_FOOTER`, so `block_len` still reads it.
        out
    }
}

/// Flush this session to the mirror before compacting, then build only from
/// the decoded backup. Failure omits the receipt; it never prevents compaction.
pub fn prepare(projects: &Path, data: &Path, source: &Path, sid: &str) -> Option<String> {
    if crate::mirror::mirror_disabled() || std::fs::metadata(source).ok()?.len() > 64 * 1024 * 1024
    {
        return None;
    }
    let captured = std::fs::metadata(source).ok()?.len();
    let storage = crate::storage::Storage::open(&data.join("csr-engine.db")).ok()?;
    let mirror = crate::mirror::Mirror::new(&storage, data, projects);
    use crate::mirror::SyncOutcome;
    if !matches!(
        mirror.sync_file(source),
        SyncOutcome::UpToDate | SyncOutcome::Appended { .. } | SyncOutcome::Rewritten { .. }
    ) {
        return None;
    }
    // A sync that defers an unfinished last line still reports success: the backup must
    // cover every byte captured, or the receipt would leave out the newest request.
    if mirror.committed_source_bytes(source)? < captured {
        return None;
    }
    let relative = source.strip_prefix(projects).ok()?;
    let mut backup = crate::mirror::transcripts_dir(data).join(relative);
    backup.set_extension("jsonl.zst");
    let parsed = super::parse_mirrored_transcript(&backup).ok()?;
    if parsed.read_error.is_some() || parsed.unrecognized_entries > 0 {
        return None;
    }
    let range = chapter(&parsed, true)?;
    Some(build(&parsed, sid, range).text())
}
