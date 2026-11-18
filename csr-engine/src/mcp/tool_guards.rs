//! Boundary checks shared by the MCP tool handlers: ids that cannot name a
//! session, chunk ids nothing stores, error envelopes that must travel as
//! errors, the 20 KB output cap, and the scratch-checkout filter of the code
//! graph.

use crate::storage::codegraph::{NeighborEdge, NodeRow};
use crate::storage::Storage;

/// Largest tool reply, in bytes, including the notice a call appends to it.
pub(super) const OUTPUT_CAP_BYTES: usize = 20 * 1024;

/// Room kept below the cap for the truncation notice.
const NOTICE_ROOM: usize = 160;

/// Room kept below the cap for the extra block `call_tool` appends when the
/// binary was replaced on disk (about 200 bytes).
const APPENDED_NOTICE_ROOM: usize = 512;

/// Most bytes of text a handler's reply carries before the notices are added.
pub(super) const TEXT_CAP_BYTES: usize = OUTPUT_CAP_BYTES - APPENDED_NOTICE_ROOM;

/// Most bytes of an error message a tool returns.
const ERROR_CAP_BYTES: usize = 2 * 1024;

/// Longest id any tool accepts.
const MAX_ID_BYTES: usize = 256;

/// Longest transcript file path `csr_transcript` accepts.
const MAX_PATH_BYTES: usize = 4096;

/// Reject a blank id, or one that is a path rather than an id.
pub(super) fn check_id(value: &str, field: &str) -> Result<(), rmcp::ErrorData> {
    super::query_validation::validate(value, field)?;
    let bad = value.len() > MAX_ID_BYTES
        || value.contains("..")
        || value.contains(['/', '\\'])
        || value.chars().any(char::is_control);
    if bad {
        return Err(not_an_id(field));
    }
    Ok(())
}

/// Like [`check_id`], for a field that may also hold the path of a transcript
/// file: a path may be long and hold slashes, but never a `..` component. A
/// value with no separator is judged as an id.
pub(super) fn check_session_ref(value: &str, field: &str) -> Result<(), rmcp::ErrorData> {
    if !value.contains(['/', '\\']) {
        return check_id(value, field);
    }
    let climbs = value.split(['/', '\\']).any(|part| part == "..");
    if value.len() > MAX_PATH_BYTES || climbs || value.chars().any(char::is_control) {
        return Err(not_an_id(field));
    }
    Ok(())
}

fn not_an_id(field: &str) -> rmcp::ErrorData {
    rmcp::ErrorData::invalid_params(format!("{field} is not a session or conversation id"), None)
}

/// The error a lookup envelope carries, when `text` is one: the not-found,
/// known-but-unreadable and ambiguous replies of the conversation tools. The
/// message is scrubbed of provider keys before it is cut, so a key cannot
/// survive by straddling the cut.
pub(super) fn envelope_error(text: &str) -> Option<rmcp::ErrorData> {
    let head = text.trim_start();
    let is_error = head.starts_with("<conversation_file>\n<error>")
        || head.starts_with("<transcript_error>")
        || head.starts_with("{\"error\"");
    is_error.then(|| {
        let scrubbed = super::redact_out(head.to_string());
        let message: String = scrubbed.chars().take(600).collect();
        rmcp::ErrorData::invalid_params(message, None)
    })
}

/// An error message scrubbed of provider keys, then cut to [`ERROR_CAP_BYTES`]
/// at a char boundary: a long argument echoed in an error stays bounded.
pub(super) fn cap_error(message: String) -> String {
    let scrubbed = super::redact_out(message);
    if scrubbed.len() <= ERROR_CAP_BYTES {
        return scrubbed;
    }
    let cut = scrubbed.floor_char_boundary(ERROR_CAP_BYTES);
    format!(
        "{} [error cut at {ERROR_CAP_BYTES} bytes]",
        &scrubbed[..cut]
    )
}

/// Fail when any of `ids` is not a stored chunk or reflection.
pub(super) fn require_known_chunks(
    storage: &Storage,
    ids: &[String],
) -> Result<(), rmcp::ErrorData> {
    for id in ids {
        check_id(id, "chunk_ids")?;
        let known = storage
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM chunks WHERE id = ?1) \
                     OR EXISTS(SELECT 1 FROM reflections WHERE id = ?1)",
                    [id],
                    |row| row.get::<_, bool>(0),
                )?)
            })
            .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;
        if !known {
            let shown: String = id.chars().take(80).collect();
            return Err(rmcp::ErrorData::invalid_params(
                format!("unknown chunk id '{shown}'"),
                None,
            ));
        }
    }
    Ok(())
}

/// `text` cut to [`TEXT_CAP_BYTES`] at a line (or char) boundary, with a
/// notice that says how much was left out. Text within the cap is unchanged.
pub(super) fn cap_output(text: String) -> String {
    if text.len() <= TEXT_CAP_BYTES {
        return text;
    }
    let room = TEXT_CAP_BYTES - NOTICE_ROOM;
    let mut cut = text.floor_char_boundary(room);
    if let Some(newline) = text[..cut].rfind('\n') {
        if newline > room / 2 {
            cut = newline;
        }
    }
    let mut out = text[..cut].to_string();
    out.push_str(&format!(
        "\n<truncated omitted_bytes=\"{}\">Output capped at 20 KB; lower the limit, narrow the request, or page with offset.</truncated>\n",
        text.len() - cut
    ));
    out
}

/// Whether `path` lies under `/tmp` or `/private/tmp`: a scratch checkout, not
/// a source anyone works in.
pub(super) fn is_scratch_path(path: &str) -> bool {
    ["/tmp", "/private/tmp"].into_iter().any(|root| {
        path == root
            || path
                .strip_prefix(root)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// Most rows asked of the code graph before the scratch sources are dropped.
/// A multiple of `limit` is not enough: scratch checkouts can hold more rows
/// than that and rank ahead of every real one, so the fetch is wide and the
/// cut to `limit` comes after the filter.
const FETCH_CEILING: usize = 100_000;

/// Rows to ask the code graph for so that `limit` remain after the scratch
/// sources are dropped.
pub(super) fn fetch_limit(limit: usize) -> usize {
    FETCH_CEILING.max(limit)
}

/// Drop the nodes under a scratch path, then keep `limit`.
pub(super) fn keep_real_nodes(nodes: &mut Vec<NodeRow>, limit: usize) {
    nodes.retain(|node| !is_scratch_path(&node.file));
    nodes.truncate(limit);
}

/// Drop the edges whose far end is under a scratch path, then keep `limit` in
/// each direction, as the storage query does: outbound edges must not use up
/// the room of the inbound ones.
pub(super) fn keep_real_edges(edges: &mut Vec<NeighborEdge>, limit: usize) {
    edges.retain(|edge| !is_scratch_path(&edge.node.file));
    let (mut outbound, mut inbound) = (0usize, 0usize);
    edges.retain(|edge| {
        let seen = if edge.direction == "out" {
            &mut outbound
        } else {
            &mut inbound
        };
        *seen += 1;
        *seen <= limit
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_that_are_paths_or_blank_are_refused() {
        assert!(check_id("0d1c2b3a-1111-4222-8333-444455556666", "session").is_ok());
        assert!(check_id("agent-ad220c1a0198fd03d", "session").is_ok());
        assert!(check_id("", "session").is_err());
        assert!(check_id("   ", "session").is_err());
        assert!(check_id("../../../../etc/passwd", "session").is_err());
        assert!(check_id("a/b", "session").is_err());
        assert!(check_id(&"x".repeat(MAX_ID_BYTES + 1), "session").is_err());
    }

    #[test]
    fn a_session_reference_may_be_a_path_but_never_climbs() {
        assert!(check_session_ref("/Users/me/.claude/projects/p/s.jsonl", "session").is_ok());
        assert!(check_session_ref("../../../../etc/passwd", "session").is_err());
        assert!(check_session_ref("  ", "session").is_err());
        assert!(check_session_ref("/a/b/../../etc/passwd", "session").is_err());
        assert!(check_session_ref("a\\..\\b", "session").is_err());
    }

    #[test]
    fn a_deep_transcript_path_is_longer_than_an_id_may_be() {
        let deep = format!("/Users/me/{}/s.jsonl", "nested-project-dir/".repeat(30));
        assert!(deep.len() > MAX_ID_BYTES);
        assert!(check_session_ref(&deep, "session").is_ok());
        assert!(check_id(&deep, "session").is_err());
        assert!(check_session_ref("/Users/me/notes..v2/s.jsonl", "session").is_ok());
        let too_long = format!("/{}", "a/".repeat(MAX_PATH_BYTES));
        assert!(check_session_ref(&too_long, "session").is_err());
        assert!(check_session_ref(&"x".repeat(MAX_ID_BYTES + 1), "session").is_err());
    }

    #[test]
    fn an_error_envelope_is_scrubbed_before_the_cut() {
        let key = format!("ghp_{}", "A".repeat(36));
        let padding = "x".repeat(564);
        // 26 chars of envelope plus the padding: the 600-char cut of the
        // unscrubbed text lands ten chars into the key.
        let text = format!("<transcript_error>\n<error>{padding}{key} tail</error>");
        let error = envelope_error(&text).unwrap();
        assert!(!error.message.contains(&key[..8]), "{}", error.message);
        assert!(error.message.len() <= 600 + 64);
    }

    #[test]
    fn a_long_error_message_is_scrubbed_and_bounded() {
        let key = format!("ghp_{}", "B".repeat(36));
        let capped = cap_error(format!("bad argument {key} {}", "é".repeat(10_000)));
        assert!(!capped.contains(&key[..8]), "{capped}");
        assert!(
            capped.len() <= ERROR_CAP_BYTES + 64,
            "{} bytes",
            capped.len()
        );
        assert_eq!(cap_error("short".to_string()), "short");
    }

    #[test]
    fn lookup_error_envelopes_become_errors_and_results_do_not() {
        let not_found = "<conversation_file>\n<error>Conversation ID 'x' not found in any project.</error>\n</conversation_file>";
        assert!(envelope_error(not_found).is_some());
        assert!(envelope_error("<transcript_error>\n<error>bad</error>").is_some());
        assert!(envelope_error("{\"error\":\"gone\"}").is_some());
        let found =
            "<conversation_file>\n<conversation_id>x</conversation_id>\n</conversation_file>";
        assert!(envelope_error(found).is_none());
        assert!(envelope_error("{\"session\":\"x\"}").is_none());
    }

    #[test]
    fn cap_keeps_small_text_and_bounds_large_text_with_a_notice() {
        assert_eq!(cap_output("short".to_string()), "short");
        let big = "ü line of text\n".repeat(5_000);
        let capped = cap_output(big.clone());
        assert!(capped.len() <= OUTPUT_CAP_BYTES, "{} bytes", capped.len());
        // Room is left for the block the binary-replaced notice adds.
        let notice = crate::binary_identity::replacement_notice("2026-11-10T00:00:00+00:00");
        assert!(capped.len() + notice.len() <= OUTPUT_CAP_BYTES);
        assert!(capped.contains("<truncated omitted_bytes="));
        let one_line = "é".repeat(30_000);
        assert!(cap_output(one_line).len() <= OUTPUT_CAP_BYTES);
    }

    #[test]
    fn scratch_paths_are_exactly_the_tmp_trees() {
        assert!(is_scratch_path("/tmp/rcperf/src/a.rs"));
        assert!(is_scratch_path("/private/tmp/csr-sbx/x/mod.rs"));
        assert!(!is_scratch_path("/tmpfoo/a.rs"));
        assert!(!is_scratch_path("/Users/me/projects/tmp/a.rs"));
        assert!(!is_scratch_path("/private/var/folders/a.rs"));
    }

    #[test]
    fn real_edges_are_kept_per_direction() {
        let edge = |direction: &str, file: &str| NeighborEdge {
            direction: direction.into(),
            edge_kind: "calls".into(),
            resolved: true,
            node: NodeRow {
                file: file.into(),
                ..NodeRow::default()
            },
        };
        let mut edges = vec![
            edge("out", "src/a.rs"),
            edge("out", "/tmp/x/b.rs"),
            edge("out", "src/c.rs"),
            edge("out", "src/d.rs"),
            edge("in", "src/e.rs"),
            edge("in", "/private/tmp/f.rs"),
            edge("in", "src/g.rs"),
            edge("in", "src/h.rs"),
        ];
        keep_real_edges(&mut edges, 2);
        let files: Vec<_> = edges.iter().map(|e| e.node.file.as_str()).collect();
        assert_eq!(files, ["src/a.rs", "src/c.rs", "src/e.rs", "src/g.rs"]);
    }

    #[test]
    fn fetch_limit_is_wide_but_never_below_the_limit() {
        assert_eq!(fetch_limit(5), FETCH_CEILING);
        assert_eq!(fetch_limit(1_000_000), 1_000_000);
    }
}
