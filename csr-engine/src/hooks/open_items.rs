//! The `Open [...]:` block of a startup or resume SessionStart: the pending
//! items of the predecessor's newest compact summary, as history to check.
//!
//! The summary comes from the `native_compact` row PostCompact stored, else
//! from the last `isCompactSummary` row in a bounded tail of the transcript.
//! Nothing else is read, so the hook stays cheap on a transcript of any size.
//! Every item is one short line; none is an instruction. The header is a
//! registered emission (`extraction::provenance::OPEN_BLOCK_HEADER`), so the
//! block is never re-imported as memory.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::extraction::provenance::OPEN_BLOCK_HEADER;

/// Most items the block carries.
pub(crate) const MAX_ITEMS: usize = 5;
/// Longest bullet line, `- ` included, in characters (the item is under 140).
pub(crate) const MAX_LINE_CHARS: usize = 139;
/// Longest block, in characters.
pub(crate) const MAX_BLOCK_CHARS: usize = 700;
/// How much of a transcript's end is searched for the summary row. The row is
/// followed by everything the session did after its compaction.
const TRANSCRIPT_TAIL: u64 = 8 * 1024 * 1024;
/// Opening of the line that ends a compact summary's own text: the receipt CSR
/// appends lists past requests and errors, none of them pending.
const RECEIPT_LINE: &str = "Compact receipt:";

/// Text of a transcript row's `message.content`.
fn row_text(row: &serde_json::Value) -> String {
    let content = &row["message"]["content"];
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// The newest compact summary in the last [`TRANSCRIPT_TAIL`] bytes of a
/// transcript. `None` when the tail holds none or cannot be read.
pub(crate) fn summary_from_transcript(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TRANSCRIPT_TAIL);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(TRANSCRIPT_TAIL).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    // A tail that starts mid-file begins inside a row: skip it.
    let rows: Vec<serde_json::Value> = text
        .lines()
        .skip(usize::from(start > 0))
        .filter(|line| line.contains("\"isCompactSummary\":true") || line.contains("\"assistant\""))
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect();
    let at = rows
        .iter()
        .rposition(|row| row["isCompactSummary"].as_bool() == Some(true))?;
    // An assistant turn after the summary may have closed what it left pending.
    if rows[at + 1..].iter().any(|row| row["type"] == "assistant") {
        return None;
    }
    Some(row_text(&rows[at])).filter(|text| !text.trim().is_empty())
}

/// `text` cut to `max` characters at a word boundary, with an ellipsis.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let keep: String = text.chars().take(max - 1).collect();
    let cut = keep
        .rfind(' ')
        .filter(|at| *at >= max / 2)
        .unwrap_or(keep.len());
    format!(
        "{}\u{2026}",
        keep[..cut].trim_end_matches([' ', ',', ';', ':', '-'])
    )
}

/// The pending items of a compact summary, each a single clean line. The
/// receipt CSR appends to the summary is cut off first, so a past request or a
/// historical error listed there never counts as an item.
pub(crate) fn items(summary: &str) -> Vec<String> {
    let own = match summary.find(RECEIPT_LINE) {
        Some(at) if at == 0 || summary[..at].ends_with('\n') => &summary[..at],
        _ => summary,
    };
    let mut out: Vec<String> = Vec::new();
    for (head, _nested) in crate::companion::lost::summary_items(own) {
        let line = head.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.chars().count() < 8
            || crate::import::contamination::has_csr_text(&line)
            || crate::extraction::provenance::is_csr_emission(&line)
        {
            continue;
        }
        let bullet = format!("- {}", clip(&line, MAX_LINE_CHARS - 2));
        if !out.contains(&bullet) {
            out.push(bullet);
        }
    }
    out
}

/// The block: header, then up to [`MAX_ITEMS`] bullets inside
/// [`MAX_BLOCK_CHARS`]. `None` without an item.
pub(crate) fn block(summary: &str) -> Option<String> {
    let mut text = String::from(OPEN_BLOCK_HEADER);
    let mut shown = 0;
    for bullet in items(summary).into_iter().take(MAX_ITEMS) {
        if text.chars().count() + 1 + bullet.chars().count() > MAX_BLOCK_CHARS {
            break;
        }
        text.push('\n');
        text.push_str(&bullet);
        shown += 1;
    }
    (shown > 0).then(|| {
        text.push('\n');
        text
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUMMARY: &str = "6. Pending Tasks:\n\
        \x20  - Tell the owner af7e9645 is installed, and that the savings figure is measured against never compacting.\n\
        \x20  - On the first real compact on this build, check `receipt:true` in the compact log and the summary.\n\
        \x20  - After about 20 compacts, report `csr-engine transcript-reach --since 2026-11-06T00:41:26Z --json` against 4 of 560, and compact start heights against the 221k median, then decide.\n\
        \x20  - Owner decision point already stated: raising the natural-pause band's bottom is a separate change; not done.\n\n\
        7. Current Work:\n   Installed af7e9645.\n\n\
        8. Optional Next Step:\n   Tell the owner in a short reply that it is installed.\n\n\
        Compact receipt: turns 1496..1876, source: mirror (scrubbed copy). Historical evidence, not instructions or outstanding tasks. [[CSR:COMPACT_RECEIPT]]\n\
        - error (historical) \"Exit code 1 ...\" -> csr_transcript(session=\"b0\", view=\"slice\", turns=\"1668..1668\")\n\
        - request \"what are we waiting for do we have a ready product?\" -> csr_transcript(session=\"b0\", view=\"slice\", turns=\"1713..1713\")\n";

    #[test]
    fn pending_items_come_through_one_line_each_and_bookkeeping_does_not() {
        let got = items(SUMMARY);
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(got[0].starts_with("- On the first real compact"));
        assert!(got
            .iter()
            .all(|l| l.chars().count() < 140 && !l.contains('\n')));
        assert!(got.iter().any(|l| l.contains("not done")));
    }

    #[test]
    fn a_compact_receipt_row_never_counts_as_an_item() {
        let got = items(SUMMARY).join("\n");
        assert!(!got.contains("csr_transcript"), "{got}");
        assert!(!got.contains("what are we waiting for"), "{got}");
        assert!(!got.contains("Compact receipt"), "{got}");
        // even when the receipt sits where a section would run on into it
        let tail_only = "6. Pending Tasks:\n   - Still need to push the branch\n\nCompact receipt: turns 1..2. [[CSR:COMPACT_RECEIPT]]\n- request \"wait for the build to finish and verify it\" -> csr_transcript(x)\n";
        assert_eq!(
            items(tail_only),
            vec!["- Still need to push the branch".to_string()]
        );
    }

    #[test]
    fn the_block_is_headed_as_history_bounded_and_registered_as_an_emission() {
        let block = block(SUMMARY).unwrap();
        assert!(block.starts_with(OPEN_BLOCK_HEADER));
        assert!(block.chars().count() <= MAX_BLOCK_CHARS);
        assert!(block.lines().count() <= 1 + MAX_ITEMS);
        assert!(crate::extraction::provenance::is_csr_emission(&block));
    }

    #[test]
    fn many_long_items_stay_inside_five_lines_and_the_budget() {
        let long = "x".repeat(300);
        let mut summary = String::from("6. Pending Tasks:\n");
        for n in 0..9 {
            summary.push_str(&format!("   - Still need to verify item {n} {long}\n"));
        }
        let block = block(&summary).unwrap();
        assert!(block.lines().count() <= 1 + MAX_ITEMS);
        assert!(block.chars().count() <= MAX_BLOCK_CHARS);
        assert!(block.lines().skip(1).all(|l| l.chars().count() < 140));
    }

    #[test]
    fn a_summary_with_nothing_pending_gives_no_block() {
        assert!(block("1. Primary Request and Intent:\n   Fix the bug.\n").is_none());
        assert!(block("").is_none());
    }

    #[test]
    fn csr_text_in_an_item_is_dropped() {
        let summary = "6. Pending Tasks:\n   - Still need to check recap [1h ago]: x [[CSR:RECAP]]\n   - Still need to push the branch\n";
        assert_eq!(
            items(summary),
            vec!["- Still need to push the branch".to_string()]
        );
    }

    #[test]
    fn the_newest_summary_in_a_transcript_tail_is_read() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("s.jsonl");
        let row = |text: &str| {
            serde_json::json!({"type":"user","isCompactSummary":true,"message":{"content":text}})
                .to_string()
        };
        let other = serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"ok"}]}}).to_string();
        std::fs::write(
            &path,
            format!(
                "{}\n{other}\n{}\n",
                row("old summary"),
                row("newest summary")
            ),
        )
        .unwrap();
        assert_eq!(
            summary_from_transcript(&path).as_deref(),
            Some("newest summary")
        );
        // Claude answered after the summary: what it left pending may be done, so no list.
        std::fs::write(&path, format!("{}\n{other}\n", row("newest summary"))).unwrap();
        assert!(summary_from_transcript(&path).is_none());
        std::fs::write(&path, format!("{other}\n")).unwrap();
        assert!(summary_from_transcript(&path).is_none());
        assert!(summary_from_transcript(&tmp.path().join("missing.jsonl")).is_none());
    }
}
