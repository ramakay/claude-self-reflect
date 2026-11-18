use super::*;
use crate::storage::witness_ledger::{self, WitnessLedgerRow};
use crate::storage::witness_verdicts::{self, VerdictKind, WitnessVerdictRow};

const EPISODE_DATA_OPEN: &str = r#"<script type="application/json" class="episode-data">"#;

fn episode_data_blocks(html: &str) -> Vec<serde_json::Value> {
    html.split(EPISODE_DATA_OPEN)
        .skip(1)
        .map(|tail| {
            let (json, _) = tail
                .split_once("</script>")
                .expect("episode-data script must be closed");
            serde_json::from_str(json).expect("episode-data must contain valid JSON")
        })
        .collect()
}

fn assert_no_external_resources(html: &str) {
    let lower = html.to_ascii_lowercase();
    for forbidden in [
        r#"src="http"#,
        r#"href="http"#,
        "cdn.",
        "integrity=",
        "<link",
        "@import",
        "url(http",
    ] {
        assert!(
            !lower.contains(forbidden),
            "report must not contain external resource reference {forbidden:?}"
        );
    }
}

/// All occurrences of `data-session="..."` (or any `needle`-prefixed
/// attribute), in document order, within `html`.
fn attr_values<'a>(html: &'a str, needle: &str) -> Vec<&'a str> {
    html.match_indices(needle)
        .filter_map(|(idx, _)| {
            let start = idx + needle.len();
            html[start..].find('"').map(|end| &html[start..start + end])
        })
        .collect()
}

/// Split the rendered report into (index-pane markup, detail-pane-wrap
/// markup) so tests can assert per-section counts without the two
/// panes' `data-session` attributes colliding.
fn split_panes(html: &str) -> (&str, &str) {
    let marker = r#"<div class="detail-pane-wrap">"#;
    let idx = html
        .find(marker)
        .expect("detail-pane-wrap marker must be present in a rendered mailbox");
    (&html[..idx], &html[idx..])
}

fn seed_episode(
    storage: &Storage,
    session_id: &str,
    timestamp: &str,
    request: &str,
    outcome: &str,
    investigated: &[&str],
    todos: &[(&str, &str)],
) {
    let content = serde_json::json!({
        "schema": "v2",
        "session_id": session_id,
        "project": "proj",
        "timestamp": timestamp,
        "request": request,
        "outcome": outcome,
        "investigated": investigated,
        "todos": todos.iter().map(|(content, status)| {
            serde_json::json!({"content": content, "status": status})
        }).collect::<Vec<_>>(),
    });
    storage
        .with_connection(|conn| {
            conn.execute(
                "INSERT INTO reflections (id, content, tags, timestamp) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    format!("episode-{session_id}"),
                    content.to_string(),
                    format!(r#"["session_episode","schema_v2","conv_{session_id}"]"#),
                    timestamp,
                ],
            )?;
            Ok(())
        })
        .unwrap();
}

/// Like `seed_episode`, but merges `extra` object keys into the episode
/// JSON — for tests exercising the Phase 2 fields (`error_count`,
/// `steer_count`, `top_errors`, `steers`, `files_modified`,
/// `error_signatures`, `completed`, `next_steps`, `blockers`) that
/// `seed_episode`'s fixed shape doesn't cover.
fn seed_episode_with_extra(
    storage: &Storage,
    session_id: &str,
    timestamp: &str,
    request: &str,
    outcome: &str,
    extra: serde_json::Value,
) {
    let mut content = serde_json::json!({
        "schema": "v2",
        "session_id": session_id,
        "project": "proj",
        "timestamp": timestamp,
        "request": request,
        "outcome": outcome,
        "investigated": [],
        "todos": [],
    });
    if let (Some(map), Some(extra_map)) = (content.as_object_mut(), extra.as_object()) {
        for (key, value) in extra_map {
            map.insert(key.clone(), value.clone());
        }
    }
    storage
        .with_connection(|conn| {
            conn.execute(
                "INSERT INTO reflections (id, content, tags, timestamp) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    format!("episode-{session_id}"),
                    content.to_string(),
                    format!(r#"["session_episode","schema_v2","conv_{session_id}"]"#),
                    timestamp,
                ],
            )?;
            Ok(())
        })
        .unwrap();
}

fn seed_registry(storage: &Storage, session_id: &str, timestamp: &str, prompt: &str) {
    storage
        .with_connection(|conn| {
            conn.execute(
                "INSERT INTO session_registry
                        (session_id, project, first_prompt, first_ts, last_ts, prompt_count)
                     VALUES (?1, 'proj', ?2, ?3, ?3, 1)",
                rusqlite::params![session_id, prompt, timestamp],
            )?;
            Ok(())
        })
        .unwrap();
}

fn seed_anchor(storage: &Storage, session_id: &str, timestamp: &str, symbol: &str) {
    storage
        .with_connection(|conn| {
            conn.execute(
                "INSERT INTO episode_anchors
                        (session_id, project, file, node_kind, name, body_hash, created_at)
                     VALUES (?1, 'proj', '/repo/src/lib.rs', 'function_item', ?2, 'hash', ?3)",
                rusqlite::params![session_id, symbol, timestamp],
            )?;
            Ok(())
        })
        .unwrap();
}

fn ledger_row(
    project: &str,
    file: &str,
    symbol: Option<&str>,
    at_oid: &str,
    stamp: &str,
) -> WitnessLedgerRow {
    WitnessLedgerRow {
        id: 0,
        project: project.into(),
        file: file.into(),
        symbol: symbol.map(|s| s.to_string()),
        span_start: symbol.map(|_| 1),
        span_end: symbol.map(|_| 3),
        stamp: stamp.into(),
        tier: "committed".into(),
        at_oid: Some(at_oid.into()),
        source_kind: "backfill".into(),
        source_id: Some(at_oid.into()),
    }
}

// ---- SINCE THEN payback panel fixtures (journal v2 Phase 5) -----------

/// Seed one `code_node_attribution` row (channel `'transcript'`) plus its
/// backing `code_nodes` row — the SINCE THEN panel's primary "happened"
/// symbol source.
fn seed_attribution(
    storage: &Storage,
    session_id: &str,
    file: &str,
    symbol: &str,
    observed_ts: &str,
) {
    storage
        .with_connection(|conn| {
            let node_id = format!("node-{session_id}-{symbol}");
            conn.execute(
                "INSERT INTO code_nodes (id, project, file, kind, name, first_conv_id, last_conv_id)
                     VALUES (?1, 'proj', ?2, 'function', ?3, '', '')",
                rusqlite::params![node_id, file, symbol],
            )?;
            conn.execute(
                "INSERT INTO code_node_attribution (node_id, channel, source_id, observed_ts, evidence)
                     VALUES (?1, 'transcript', ?2, ?3, 'coedit_event')",
                rusqlite::params![node_id, session_id, observed_ts],
            )?;
            Ok(())
        })
        .unwrap();
}

/// Seed a `witness_ledger` row for `(project, file, symbol)` and return
/// its id — the "witnessed" evidence behind a `witnessed_unverified`
/// conclusion.
fn seed_witness(storage: &Storage, project: &str, file: &str, symbol: &str, at_oid: &str) -> i64 {
    storage
        .insert_witness(&ledger_row(
            project,
            file,
            Some(symbol),
            at_oid,
            &format!("b3:{symbol}"),
        ))
        .unwrap();
    storage
        .witnesses_for_file(project, file)
        .unwrap()
        .into_iter()
        .find(|w| w.symbol.as_deref() == Some(symbol))
        .unwrap()
        .id
}

/// Seed one `witness_verdicts` event at an explicit `created_at` — direct
/// SQL because `Storage::insert_witness_verdict` always stamps
/// `datetime('now')`, and SINCE THEN's date-ordering / day-digest tests
/// need deterministic dates.
fn seed_verdict_event(
    storage: &Storage,
    witness_id: i64,
    verdict: &str,
    receipt_oid: Option<&str>,
    created_at: &str,
) {
    storage
        .with_connection(|conn| {
            conn.execute(
                "INSERT INTO witness_verdicts
                        (witness_id, verdict, successor_witness_id, receipt_oid, observed_head_oid, created_at)
                     VALUES (?1, ?2, NULL, ?3, 'head', ?4)",
                rusqlite::params![witness_id, verdict, receipt_oid, created_at],
            )?;
            Ok(())
        })
        .unwrap();
}

/// Full pipeline (gather -> curate with no AI invocation -> finalize ->
/// render), the shape every test below needs. Curation always runs with
/// `allow_invoke=false` so tests never spawn `claude` or race the
/// process-global `CSR_NO_AI_NARRATIVES` variable. Uses a fresh, empty
/// tempdir as `projects_dir` — the report-time backfill resolves no
/// transcript against it and simply no-ops, which is what every test
/// that doesn't care about instrumentation backfill wants. Tests that
/// DO care use `render_with_projects_dir` directly.
fn render(storage: &Storage, mode: crate::storage::recap_feeds::ConsumptionMode) -> String {
    let empty_dir = tempfile::tempdir().unwrap();
    render_with_projects_dir(storage, empty_dir.path(), mode)
}

fn render_with_projects_dir(
    storage: &Storage,
    projects_dir: &Path,
    mode: crate::storage::recap_feeds::ConsumptionMode,
) -> String {
    let mut raw = gather_report_data_with(storage, projects_dir, mode).unwrap();
    curate_sentences_with(storage, &mut raw.rich_cards, false);
    let data = finalize_report_data(raw);
    render_html(&data).unwrap()
}

fn seed_rich_sessions(storage: &Storage, count: usize) {
    for index in 0..count {
        let session = format!("rich-session-{index:03}");
        let timestamp = format!("2026-04-{:02}T0{}:00:00Z", (index % 27) + 1, index % 9);
        seed_episode(
            storage,
            &session,
            &timestamp,
            &format!("request number {index}"),
            "done",
            &[],
            &[],
        );
    }
}

/// Returns the indices of `cards` that would still need a `claude -p`
/// call: those whose `sentence_hash` has no matching, non-empty-
/// description row in `journal_headlines`. Test-only projection of the
/// same predicate `curate_sentences_with`'s cache-application loop uses.
fn curation_misses(storage: &Storage, cards: &[StoryCardView]) -> Vec<usize> {
    cards
        .iter()
        .enumerate()
        .filter_map(|(index, card)| {
            let hash = sentence_hash(card);
            cached_sentence(storage, &card.session_short, &hash)
                .is_none()
                .then_some(index)
        })
        .collect()
}

/// Helper: the `model` `journal_headlines` stored a card's row under.
fn cached_model(storage: &Storage, session_short: &str) -> Option<String> {
    storage
        .with_connection(|conn| {
            Ok(conn
                .query_row(
                    "SELECT model FROM journal_headlines WHERE session_id = ?1",
                    rusqlite::params![session_short],
                    |row| row.get::<_, String>(0),
                )
                .ok())
        })
        .unwrap()
}

#[path = "report_tests/mixed_tests.rs"]
mod mixed_tests;

#[path = "report_tests/render_tests.rs"]
mod render_tests;
