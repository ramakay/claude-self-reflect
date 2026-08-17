//! Journal v4 P4b — dream → outcome attribution.
//!
//! Every copy block the journal renders ends with ONE opaque marker line
//! carrying a short dream id and nothing else. When the user pastes that
//! prompt into a fresh session, the marker travels with it; the importer sees
//! it, and CSR learns which of its own dreams caused work.
//!
//! # The one rule
//!
//! **Binding is evidence.** A marker in an imported transcript proves the
//! prompt was used. Its absence proves nothing at all — not that the dream
//! was ignored, not that it was acted on by other means. So an unbound dream
//! renders NOTHING about outcomes, and there is no code path in this module
//! that writes a binding without a marker.
//!
//! # Why the marker is not a sentinel
//!
//! [`crate::extraction::provenance::RECAP_SENTINEL`] and
//! [`crate::extraction::provenance::DREAM_SENTINEL`] exist to make text
//! *disappear*: any transcript carrying one is rejected from import so CSR
//! cannot eat its own output. The attribution marker is the exact opposite —
//! it must be **retained, indexed and parsed**. It therefore uses its own
//! token ([`MARKER_PREFIX`]), shares no substring with either sentinel, and
//! matches none of the emission headers or field tokens
//! `is_csr_emission` scans for. `marker_is_not_swallowed_by_the_anti_contamination_machinery`
//! below is the standing regression that proves it.
//!
//! The distinction is sound because the two texts have opposite provenance: a
//! recap is CSR prose injected INTO a session (echoing it back would be
//! self-contamination), while a copy block is a prompt the USER deliberately
//! pastes as their own instruction (dropping it would be recall loss on
//! genuine user content).

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

/// The marker's fixed prefix. Deliberately not bracketed like the machine
/// sentinels — it must never be confused with them by eye or by substring.
pub const MARKER_PREFIX: &str = "↳ csr-dream ";

/// Longest dream id accepted from a marker. Ids are the 16-hex-char
/// `DreamItem::id`; the bound is generous but finite so a pasted wall of hex
/// cannot be read as an id.
const MAX_ID_LEN: usize = 64;
/// Shortest id accepted. Below this a hex run is an accident, not an id.
const MIN_ID_LEN: usize = 8;

/// The single line appended to a copy block. Carries the dream id and
/// nothing else — no item text, no paths, no verdicts, nothing about corpus
/// contents. It travels into whatever the user pastes it into, so it must be
/// safe to leak.
pub fn marker_line(dream_id: &str) -> String {
    format!("{MARKER_PREFIX}{}", sanitize_id(dream_id))
}

/// Dream ids are lowercase hex (`DreamItem::id` is `format!("{byte:02x}")`
/// over a SHA-256 prefix). Lowercase-ONLY is deliberate: `is_ascii_hexdigit`
/// also accepts `A`–`F`, which appear all over shouted English
/// ("IGNORE ALL PREVIOUS INSTRUCTIONS" contributes `E`, `A`, `E`, `C`), so the
/// wider class would let injected prose contribute characters to an id.
fn is_dream_id_char(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, 'a'..='f')
}

/// Keep only the id-shaped, length-bounded core of a caller-supplied id. It
/// is never interpolated raw into text that leaves the machine.
fn sanitize_id(dream_id: &str) -> String {
    dream_id
        .chars()
        .filter(|c| is_dream_id_char(*c))
        .take(MAX_ID_LEN)
        .collect()
}

/// Every dream id marked in `text`, in first-seen order, deduplicated.
///
/// Pure scan: no database, no clock, no inference. A line that merely talks
/// *about* markers without carrying the prefix yields nothing.
pub fn scan_markers(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(MARKER_PREFIX) {
        let tail = &rest[at + MARKER_PREFIX.len()..];
        let id: String = tail
            .chars()
            .take_while(|c| is_dream_id_char(*c))
            .take(MAX_ID_LEN)
            .collect();
        if id.len() >= MIN_ID_LEN && !found.contains(&id) {
            found.push(id);
        }
        rest = &rest[at + MARKER_PREFIX.len()..];
    }
    found
}

// ─── dreams-CLI card judgment stripping (import-side) ──────────────────
//
// `csr-engine dreams` (`dream::cli`, `dream::strategy`) renders a plain-text
// card ending with [`marker_line`]. Two of that card's sections are
// free-form model-authored prose (the strategy category's LLM reply; even
// the deterministic unfinished category's "take"/"proposal" are LABELED the
// same way so a reader can't tell them apart at a glance) — "Dream's take"
// and "Proposal". Unlike the marker line itself, that prose is NOT meant to
// be retained: if a user pastes a card into a new session, the model's own
// judgment must not re-embed as if it were the user's stated intent. Ordinary
// facts (`Observed:` bullets, receipt-anchored) and deterministic homework
// (`Verify first:`) are not gated here — only the two judgment/directive
// sections whose content genuinely originates from a model call.
//
// This intentionally does NOT go through `is_csr_emission`'s header
// registry: that machinery REJECTS a whole text outright, which would also
// discard the marker line and break attribution. This is span-level
// suppression instead — the marker line survives untouched so
// `import::dream_marker::bind_markers` still fires on the parsed chunk.

/// Card-grammar headers that open the two model-authored spans a pasted
/// dreams-CLI card carries. Single source of truth: [`crate::dream::cli`]
/// and [`crate::dream::strategy`] format their cards with these exact
/// strings, and [`strip_card_judgment`] recognizes exactly these strings —
/// a mismatch here is a silent contamination hole, not a compile error, so
/// any change to either formatter's header text MUST update these too.
pub const DREAM_CARD_TAKE_HEADER: &str = "Dream's take — not a fact:";
pub const DREAM_CARD_PROPOSAL_HEADER: &str = "Proposal — requires verdict:";

/// Headers that end a redacted span when encountered mid-card: the other
/// judgment header (so a Take span stops where a Proposal begins), the
/// deterministic homework header, a fresh card's own leading header, and the
/// marker line that closes every card.
const DREAM_CARD_SPAN_BOUNDARIES: [&str; 4] = [
    DREAM_CARD_PROPOSAL_HEADER,
    "Verify first:",
    "PROJECT ",
    MARKER_PREFIX,
];

/// Strip the free-form "Dream's take"/"Proposal" spans out of any pasted
/// dreams-CLI card found in `text`, replacing each with just its header (no
/// prose), while leaving everything else — crucially [`MARKER_PREFIX`] lines
/// — untouched. Cheap no-op when `text` carries no marker at all: a card can
/// only ever be recognized as one if [`MARKER_PREFIX`] closes it, so text
/// that merely happens to use one of these header phrases in ordinary prose
/// is never touched (avoids false positives on genuine writing about "the
/// proposal" or "my take").
///
/// A "span" runs from its header line to (but not including) the next
/// [`DREAM_CARD_SPAN_BOUNDARIES`] match, so a multi-line paragraph is
/// suppressed in full, not just its header line.
pub fn strip_card_judgment(text: &str) -> String {
    if !text.contains(MARKER_PREFIX) {
        return text.to_string();
    }

    let judgment_headers = [DREAM_CARD_TAKE_HEADER, DREAM_CARD_PROPOSAL_HEADER];
    let mut out = String::with_capacity(text.len());
    let mut redacting = false;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if let Some(header) = judgment_headers.iter().find(|h| trimmed.starts_with(**h)) {
            redacting = true;
            out.push_str(header);
            out.push('\n');
            continue;
        }
        if redacting {
            if DREAM_CARD_SPAN_BOUNDARIES
                .iter()
                .any(|b| trimmed.starts_with(b))
            {
                redacting = false;
                // fall through: this line itself is not part of the span
            } else {
                continue;
            }
        }
        out.push_str(line);
    }
    out
}

/// One dream's attribution as stored. Only ever constructed from a row that
/// carries `bound_session_id` — an unbound dream has no `DreamAttribution`,
/// which is what makes "renders nothing about outcomes" structural rather
/// than a rendering convention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DreamAttribution {
    pub dream_id: String,
    /// The prompt kind that was emitted, when an emission row recorded it.
    /// `None` is honest: the marker carries an id and nothing else, so a
    /// binding without a matching emission genuinely does not know.
    pub kind: Option<String>,
    pub emitted_at: Option<String>,
    /// The transcript that carried the marker. Present by construction.
    pub bound_session_id: String,
    pub bound_at: String,
    pub outcome_episode_id: Option<String>,
    pub outcome: Option<String>,
    pub receipts: Vec<String>,
}

/// Record that a copy block of `kind` carrying `dream_id`'s marker was
/// rendered. `INSERT OR IGNORE`: re-rendering the same block does not
/// re-emit. An emission is NOT evidence of use — it only lets a later
/// binding name the prompt kind it came from.
///
/// **Never call this from a GET route.** The journal server is read-only
/// except for the explicit resolve/dismiss POSTs (plan, "Security"), and a
/// page view is not a copy. The natural caller is the copy action itself.
/// Until one exists, bindings simply carry `kind = NULL`, which every reader
/// here already treats as "not known" rather than guessing.
pub fn record_emission(conn: &Connection, dream_id: &str, kind: &str) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO dream_attributions (dream_id, kind, emitted_at)
         VALUES (?1, ?2, datetime('now'))",
        params![dream_id, kind],
    )?;
    Ok(())
}

/// Bind `dream_id` to the session whose transcript carried its marker.
///
/// Returns `true` when this call created the binding. `INSERT OR IGNORE`
/// against `UNIQUE(dream_id, bound_session_id)` makes a re-import idempotent:
/// the same transcript scanned twice binds once.
///
/// `kind` is copied from the newest emission row when one exists, and left
/// NULL when none does. It is never guessed.
pub fn bind_marker(conn: &Connection, dream_id: &str, session_id: &str) -> Result<bool> {
    let kind: Option<String> = conn
        .query_row(
            "SELECT kind FROM dream_attributions
             WHERE dream_id = ?1 AND bound_session_id IS NULL
             ORDER BY id DESC LIMIT 1",
            params![dream_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let emitted_at: Option<String> = conn
        .query_row(
            "SELECT emitted_at FROM dream_attributions
             WHERE dream_id = ?1 AND bound_session_id IS NULL
             ORDER BY id DESC LIMIT 1",
            params![dream_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let changed = conn.execute(
        "INSERT OR IGNORE INTO dream_attributions
            (dream_id, kind, emitted_at, bound_session_id, bound_at)
         VALUES (?1, ?2, ?3, ?4, datetime('now'))",
        params![dream_id, kind, emitted_at, session_id],
    )?;
    Ok(changed > 0)
}

/// Attach a measured outcome to an EXISTING binding. Returns `false` when no
/// binding exists — an outcome may never create one, because that would
/// record a consequence for a dream nothing proved was used.
pub fn record_outcome(
    conn: &Connection,
    dream_id: &str,
    session_id: &str,
    episode_id: &str,
    outcome: Option<&str>,
    receipts: &[String],
) -> Result<bool> {
    let updated = conn.execute(
        "UPDATE dream_attributions
            SET outcome_episode_id = ?3, outcome = ?4, receipts_json = ?5
          WHERE dream_id = ?1 AND bound_session_id = ?2",
        params![
            dream_id,
            session_id,
            episode_id,
            outcome,
            serde_json::to_string(receipts)?,
        ],
    )?;
    Ok(updated > 0)
}

/// Fill in the outcome of an already-bound dream from the bound session's
/// stored v2 episode, if one exists yet.
///
/// Deterministic and evidence-only: the outcome string is the episode's own
/// `outcome` field, and `outcome_episode_id` is that episode's row id. When
/// the session has no episode yet (the common case at import time — episodes
/// are written when the session ends) nothing is recorded and the binding
/// keeps saying only "this was pasted". A later pass fills it in.
///
/// Returns `true` when an outcome was written by this call.
pub fn refresh_outcome(conn: &Connection, dream_id: &str) -> Result<bool> {
    let Some(attribution) = load_attribution(conn, dream_id)? else {
        return Ok(false); // unbound: nothing to attach an outcome to
    };
    if attribution.outcome.is_some() {
        return Ok(false); // already measured; never rewritten
    }
    let episode: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT id, json_extract(content, '$.outcome') FROM reflections
             WHERE json_valid(content)
               AND json_extract(content, '$.schema') = 'v2'
               AND json_extract(content, '$.session_id') = ?1
             ORDER BY rowid DESC LIMIT 1",
            params![attribution.bound_session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((episode_id, outcome)) = episode else {
        return Ok(false);
    };
    let outcome = outcome
        .map(|o| o.trim().to_string())
        .filter(|o| !o.is_empty());
    if outcome.is_none() {
        // The episode exists but recorded no outcome. Writing the episode id
        // with a NULL outcome would render "acted on … → outcome" with
        // nothing behind it, so nothing is written at all.
        return Ok(false);
    }
    record_outcome(
        conn,
        dream_id,
        &attribution.bound_session_id,
        &episode_id,
        outcome.as_deref(),
        &[],
    )
}

/// The newest marker-backed binding for `dream_id`, or `None`.
///
/// `None` is the answer for an unbound dream AND for a dream that was only
/// ever emitted. Callers therefore cannot render an outcome for anything the
/// corpus did not witness.
pub fn load_attribution(conn: &Connection, dream_id: &str) -> Result<Option<DreamAttribution>> {
    let mut stmt = conn.prepare(
        "SELECT dream_id, kind, emitted_at, bound_session_id, bound_at,
                outcome_episode_id, outcome, receipts_json
         FROM dream_attributions
         WHERE dream_id = ?1 AND bound_session_id IS NOT NULL
         ORDER BY id DESC LIMIT 1",
    )?;
    let mut rows = stmt.query(params![dream_id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let receipts_json: String = row.get(7)?;
    Ok(Some(DreamAttribution {
        dream_id: row.get(0)?,
        kind: row.get(1)?,
        emitted_at: row.get(2)?,
        bound_session_id: row.get(3)?,
        bound_at: row.get(4)?,
        outcome_episode_id: row.get(5)?,
        outcome: row.get(6)?,
        receipts: serde_json::from_str(&receipts_json).unwrap_or_default(),
    }))
}

/// Measured attribution totals for `status`. Every field counts rows that
/// exist. `emitted` counts dreams whose copy block was rendered; `bound`
/// counts those a marker proved were pasted. The difference is NOT
/// "ignored" — it is "no evidence either way", and the caller must label it
/// that way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AttributionCounts {
    pub emitted: i64,
    pub bound: i64,
    pub with_outcome: i64,
}

/// Count emissions, bindings and bound-with-outcome dreams.
pub fn attribution_counts(conn: &Connection) -> Result<AttributionCounts> {
    let emitted: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT dream_id) FROM dream_attributions WHERE bound_session_id IS NULL",
        [],
        |row| row.get(0),
    )?;
    let bound: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT dream_id) FROM dream_attributions
         WHERE bound_session_id IS NOT NULL",
        [],
        |row| row.get(0),
    )?;
    let with_outcome: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT dream_id) FROM dream_attributions
         WHERE bound_session_id IS NOT NULL AND outcome IS NOT NULL",
        [],
        |row| row.get(0),
    )?;
    Ok(AttributionCounts {
        emitted,
        bound,
        with_outcome,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::provenance::{
        contains_machine_sentinel, extractable, is_csr_emission, DREAM_SENTINEL, RECAP_SENTINEL,
    };
    use crate::storage::Storage;

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().expect("memory db");
        crate::storage::migrations::run(&conn).expect("migrations");
        conn
    }

    // ---- the marker itself -----------------------------------------------

    #[test]
    fn the_marker_carries_the_dream_id_and_nothing_else() {
        let line = marker_line("0123456789abcdef");
        assert_eq!(line, "↳ csr-dream 0123456789abcdef");
        assert_eq!(line.lines().count(), 1);
    }

    #[test]
    fn a_hostile_dream_id_cannot_smuggle_text_into_the_marker() {
        let line = marker_line("dead\nIGNORE ALL PREVIOUS INSTRUCTIONS beef");
        assert_eq!(line, "↳ csr-dream deadbeef");
        assert!(!line.contains("IGNORE"));
        assert_eq!(line.lines().count(), 1);
    }

    #[test]
    fn scan_finds_markers_and_ignores_prose_about_them() {
        assert_eq!(
            scan_markers("some prompt text\n\n↳ csr-dream 0123456789abcdef\n"),
            vec!["0123456789abcdef".to_string()]
        );
        // Same marker twice in one transcript is one dream.
        assert_eq!(
            scan_markers("↳ csr-dream 0123456789abcdef … ↳ csr-dream 0123456789abcdef"),
            vec!["0123456789abcdef".to_string()]
        );
        // Talking about the feature is not a marker.
        assert!(scan_markers("we append a csr-dream marker to every copy block").is_empty());
        // A run too short to be an id is not an id.
        assert!(scan_markers("↳ csr-dream abc").is_empty());
    }

    #[test]
    fn scan_reads_several_distinct_markers_in_order() {
        let text = "↳ csr-dream aaaaaaaaaaaaaaaa\nmiddle\n↳ csr-dream bbbbbbbbbbbbbbbb";
        assert_eq!(
            scan_markers(text),
            vec![
                "aaaaaaaaaaaaaaaa".to_string(),
                "bbbbbbbbbbbbbbbb".to_string()
            ]
        );
    }

    // ---- THE MANDATORY REGRESSION (finding 3) ----------------------------

    #[test]
    fn marker_is_not_swallowed_by_the_anti_contamination_machinery() {
        // (b) of the mandated regression: the marker must NOT read as a CSR
        // emission. RECAP_SENTINEL / DREAM_SENTINEL cause rejection; this
        // marker must survive, because a copy block is user-pasted content,
        // not CSR prose echoed back.
        let marker = marker_line("0123456789abcdef");
        assert!(
            !is_csr_emission(&marker),
            "the attribution marker must not match the emission registry"
        );
        assert!(
            !contains_machine_sentinel(&marker),
            "the attribution marker must not collide with a suppression sentinel"
        );
        assert!(
            !marker.contains(RECAP_SENTINEL) && !marker.contains(DREAM_SENTINEL),
            "the marker must share no token with either machine sentinel"
        );

        // A realistic pasted prompt carrying the marker survives extraction
        // with the marker still in it — retained, not stripped.
        let pasted = format!(
            "## Resume: finish the release gate\n\n\
             This todo was left open on 2026-08-01 in session `sess-1`.\n\n\
             ### Files to look at\n\n- `csr-engine/src/dream/report.rs`\n\n\
             {marker}\n"
        );
        let kept = extractable(&pasted).expect("a pasted copy block must still be extractable");
        assert!(
            kept.contains(MARKER_PREFIX) && kept.contains("0123456789abcdef"),
            "the marker must survive the provenance pipeline intact:\n{kept}"
        );
        assert_eq!(
            scan_markers(&kept),
            vec!["0123456789abcdef".to_string()],
            "and must still be parseable after extraction"
        );
    }

    #[tokio::test]
    async fn a_marker_bearing_transcript_still_imports_and_embeds_normally() {
        // (a) of the mandated regression, end to end through the real engine:
        // parse → embed → store. If the marker were treated as a sentinel the
        // whole conversation would be dropped and this count would be zero.
        let dir = tempfile::tempdir().expect("tempdir");
        let transcript = dir.path().join("sess-marker.jsonl");
        let marker = marker_line("0123456789abcdef");
        let body = format!(
            "Resume the release gate work. Everything below is copied from stored rows. {marker}"
        );
        let line = serde_json::json!({
            "type": "user",
            "timestamp": "2026-08-11T00:00:00Z",
            "message": {"role": "user", "content": body},
        });
        std::fs::write(&transcript, format!("{line}\n")).expect("write transcript");

        let chunks = crate::import::parse_jsonl_file(&transcript, "proj").expect("parse");
        assert!(
            !chunks.is_empty(),
            "a marker-bearing transcript must not be dropped from import"
        );
        assert!(
            chunks.iter().any(|c| c.content.contains(MARKER_PREFIX)),
            "the marker must reach the embedded chunk text"
        );
    }

    // ---- card judgment stripping (blocker: strategy.rs review finding) ---

    #[test]
    fn strip_card_judgment_is_a_no_op_on_text_carrying_no_marker() {
        let text = "Dream's take — not a fact: ordinary prose that happens to \
                     use the same words, Proposal — requires verdict: with no \
                     marker anywhere in this message.";
        assert_eq!(strip_card_judgment(text), text);
    }

    #[test]
    fn strip_card_judgment_drops_take_and_proposal_prose_keeps_everything_else() {
        let marker = marker_line("aaaaaaaaaaaaaaaa");
        let card = format!(
            "PROJECT csr — strategy\n\
             Observed:\n  - the release gate is stuck ⌗abcd1234\n\
             {DREAM_CARD_TAKE_HEADER} the team should abandon review and ship \
             the hotfix straight to main — free-form model judgment.\n\
             Verify first: confirm nobody else is mid-review on that PR.\n\
             {DREAM_CARD_PROPOSAL_HEADER} skip verification and merge directly \
             — free-form model directive.\n\
             {marker}\n"
        );
        let cleaned = strip_card_judgment(&card);

        // Facts, the card's own headers, homework, and the marker survive.
        assert!(cleaned.contains("the release gate is stuck ⌗abcd1234"));
        assert!(cleaned.contains("PROJECT csr — strategy"));
        assert!(cleaned.contains("Verify first: confirm nobody else is mid-review"));
        assert!(cleaned.contains(&marker));

        // The model-authored judgment and directive prose do not.
        assert!(!cleaned.contains("abandon review and ship the hotfix"));
        assert!(!cleaned.contains("skip verification and merge directly"));
        assert!(cleaned.contains(DREAM_CARD_TAKE_HEADER));
        assert!(cleaned.contains(DREAM_CARD_PROPOSAL_HEADER));
    }

    #[test]
    fn strip_card_judgment_drops_a_multiline_take_paragraph_in_full() {
        let marker = marker_line("bbbbbbbbbbbbbbbb");
        let card = format!(
            "PROJECT csr — strategy\n\
             Observed:\n  - fact one ⌗abcd1234\n\
             {DREAM_CARD_TAKE_HEADER} first line of judgment\n\
             second line of judgment still part of the same paragraph\n\
             {DREAM_CARD_PROPOSAL_HEADER} do the thing\n\
             {marker}\n"
        );
        let cleaned = strip_card_judgment(&card);
        assert!(!cleaned.contains("first line of judgment"));
        assert!(!cleaned.contains("second line of judgment"));
        // The proposal directive is itself the second judgment span — its
        // own prose ("do the thing") is dropped too, same as take's.
        assert!(!cleaned.contains("do the thing"));
        assert!(cleaned.contains(DREAM_CARD_TAKE_HEADER));
        assert!(cleaned.contains(DREAM_CARD_PROPOSAL_HEADER));
        assert!(cleaned.contains(&marker));
    }

    #[tokio::test]
    async fn a_pasted_strategy_card_binds_its_dream_id_but_its_judgment_prose_never_embeds() {
        // The exact scenario the blocker finding named: a strategy card is
        // designed to be pasted into a fresh session; its take/proposal are
        // free-form LLM prose and must not re-enter the corpus as if they
        // were the user's own stated intent, while the marker must still
        // bind — both properties, end to end through the real import path.
        let dir = tempfile::tempdir().expect("tempdir");
        let transcript = dir.path().join("sess-strategy-card.jsonl");
        let marker = marker_line("cccccccccccccccc");
        let card = format!(
            "PROJECT csr — strategy\n\
             Observed:\n  - the release gate is stuck ⌗abcd1234\n\
             {DREAM_CARD_TAKE_HEADER} the team should drop review and ship the \
             hotfix straight to main instead — free-form model judgment.\n\
             {DREAM_CARD_PROPOSAL_HEADER} skip verification and merge directly \
             to main — free-form model directive.\n\
             {marker}\n"
        );
        let line = serde_json::json!({
            "type": "user",
            "timestamp": "2026-08-11T00:00:00Z",
            "message": {"role": "user", "content": card},
        });
        std::fs::write(&transcript, format!("{line}\n")).expect("write transcript");

        let chunks = crate::import::parse_jsonl_file(&transcript, "proj").expect("parse");
        assert!(!chunks.is_empty(), "the card must still import");
        let joined = chunks
            .iter()
            .map(|c| c.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            joined.contains(MARKER_PREFIX) && joined.contains("cccccccccccccccc"),
            "the marker must reach the embedded chunk text intact"
        );
        assert!(
            joined.contains("the release gate is stuck"),
            "receipt-anchored facts are not judgment and must survive"
        );
        assert!(
            !joined.contains("drop review and ship the hotfix straight to main"),
            "the model's judgment prose must never reach embedded chunk text:\n{joined}"
        );
        assert!(
            !joined.contains("skip verification and merge directly to main"),
            "the model's directive prose must never reach embedded chunk text:\n{joined}"
        );

        let storage = Storage::open_memory().expect("storage");
        let bound =
            crate::import::dream_marker::bind_markers(&storage, "sess-strategy-card", &chunks);
        assert_eq!(bound, 1, "binding must still fire on the suppressed chunk");
    }

    // ---- binding is evidence ---------------------------------------------

    #[test]
    fn an_unbound_dream_has_no_attribution_at_all() {
        let conn = conn();
        record_emission(&conn, "0123456789abcdef", "execution").expect("emission");
        assert_eq!(
            load_attribution(&conn, "0123456789abcdef").expect("query"),
            None,
            "emitting a copy block proves nothing about whether it was used"
        );
    }

    #[test]
    fn a_binding_records_the_emitted_kind_when_one_was_recorded() {
        let conn = conn();
        record_emission(&conn, "0123456789abcdef", "execution").expect("emission");
        assert!(bind_marker(&conn, "0123456789abcdef", "sess-9").expect("bind"));
        let attribution = load_attribution(&conn, "0123456789abcdef")
            .expect("query")
            .expect("bound");
        assert_eq!(attribution.bound_session_id, "sess-9");
        assert_eq!(attribution.kind.as_deref(), Some("execution"));
        assert!(attribution.emitted_at.is_some());
        assert_eq!(attribution.outcome, None);
    }

    #[test]
    fn a_binding_without_an_emission_row_says_the_kind_is_unknown() {
        let conn = conn();
        assert!(bind_marker(&conn, "0123456789abcdef", "sess-9").expect("bind"));
        let attribution = load_attribution(&conn, "0123456789abcdef")
            .expect("query")
            .expect("bound");
        assert_eq!(
            attribution.kind, None,
            "the marker carries an id only — the kind must not be guessed"
        );
    }

    #[test]
    fn re_importing_the_same_transcript_binds_once() {
        let conn = conn();
        assert!(bind_marker(&conn, "0123456789abcdef", "sess-9").expect("first"));
        assert!(
            !bind_marker(&conn, "0123456789abcdef", "sess-9").expect("second"),
            "a re-import must not double-count a binding"
        );
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM dream_attributions WHERE bound_session_id = 'sess-9'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(count, 1);
    }

    #[test]
    fn an_outcome_cannot_create_a_binding_that_no_marker_proved() {
        let conn = conn();
        record_emission(&conn, "0123456789abcdef", "execution").expect("emission");
        let recorded = record_outcome(
            &conn,
            "0123456789abcdef",
            "sess-9",
            "ep-1",
            Some("completed"),
            &["aaaaaaaa".to_string()],
        )
        .expect("outcome");
        assert!(
            !recorded,
            "an outcome may never manufacture the binding it depends on"
        );
        assert_eq!(
            load_attribution(&conn, "0123456789abcdef").expect("query"),
            None
        );
    }

    #[test]
    fn an_outcome_attaches_to_a_marker_backed_binding() {
        let conn = conn();
        bind_marker(&conn, "0123456789abcdef", "sess-9").expect("bind");
        assert!(record_outcome(
            &conn,
            "0123456789abcdef",
            "sess-9",
            "ep-1",
            Some("completed"),
            &["aaaaaaaabbbbbbbb".to_string()],
        )
        .expect("outcome"));
        let attribution = load_attribution(&conn, "0123456789abcdef")
            .expect("query")
            .expect("bound");
        assert_eq!(attribution.outcome.as_deref(), Some("completed"));
        assert_eq!(attribution.outcome_episode_id.as_deref(), Some("ep-1"));
        assert_eq!(attribution.receipts, vec!["aaaaaaaabbbbbbbb".to_string()]);
    }

    #[test]
    fn refresh_outcome_never_invents_a_binding_or_rewrites_a_measured_one() {
        let conn = conn();
        // Unbound: nothing to attach to, whatever episodes exist.
        conn.execute(
            "INSERT INTO reflections (id, content, tags, timestamp)
             VALUES ('ep-1', ?1, '[]', '2026-08-11T00:00:00Z')",
            params![r#"{"schema":"v2","session_id":"sess-9","outcome":"completed"}"#],
        )
        .expect("episode");
        assert!(!refresh_outcome(&conn, "0123456789abcdef").expect("unbound"));
        assert_eq!(
            load_attribution(&conn, "0123456789abcdef").expect("query"),
            None
        );

        // Bound: the outcome lands once.
        bind_marker(&conn, "0123456789abcdef", "sess-9").expect("bind");
        assert!(refresh_outcome(&conn, "0123456789abcdef").expect("first"));
        assert!(
            !refresh_outcome(&conn, "0123456789abcdef").expect("second"),
            "a measured outcome is never rewritten"
        );
        let attribution = load_attribution(&conn, "0123456789abcdef")
            .expect("query")
            .expect("bound");
        assert_eq!(attribution.outcome.as_deref(), Some("completed"));
        assert_eq!(attribution.outcome_episode_id.as_deref(), Some("ep-1"));
    }

    #[test]
    fn counts_report_emitted_and_bound_separately() {
        let conn = conn();
        record_emission(&conn, "aaaaaaaaaaaaaaaa", "execution").expect("emit a");
        record_emission(&conn, "bbbbbbbbbbbbbbbb", "housekeeping").expect("emit b");
        bind_marker(&conn, "aaaaaaaaaaaaaaaa", "sess-1").expect("bind a");
        record_outcome(
            &conn,
            "aaaaaaaaaaaaaaaa",
            "sess-1",
            "ep-1",
            Some("completed"),
            &[],
        )
        .expect("outcome a");

        let counts = attribution_counts(&conn).expect("counts");
        assert_eq!(counts.emitted, 2);
        assert_eq!(counts.bound, 1);
        assert_eq!(counts.with_outcome, 1);
    }

    #[test]
    fn attribution_survives_a_storage_round_trip() {
        let storage = Storage::open_memory().expect("storage");
        storage
            .with_connection(|conn| {
                record_emission(conn, "0123456789abcdef", "investigative")?;
                bind_marker(conn, "0123456789abcdef", "sess-3")?;
                Ok(())
            })
            .expect("write");
        let loaded = storage
            .with_connection(|conn| load_attribution(conn, "0123456789abcdef"))
            .expect("read")
            .expect("bound");
        assert_eq!(loaded.kind.as_deref(), Some("investigative"));
    }
}
