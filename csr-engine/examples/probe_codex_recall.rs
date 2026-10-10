//! WP-A stage probe: where does a target conversation drop out of
//! `csr_reflect_on_past`, and does the miss generalize across the corpus?
//!
//! Runs the REAL pipeline (`csr_engine::mcp::tools::reflect_on_past`)
//! instrumented via `csr_engine::mcp::stage_probe`, never a reimplementation
//! of it — see that module's doc for why a reimplementation would risk
//! silently drifting from production behavior. Read-only against whatever
//! `--db` points at; this repo's convention is a `/tmp` CLONE
//! (`VACUUM INTO` + a copy of the index dir), never the live database.
//!
//!   cargo run --release --example probe_codex_recall -- \
//!       --db /tmp/csr-wpa/csr-engine.db \
//!       --projects-dir /Users/you/.claude/projects \
//!       --sample 20
//!
//! Optional `--paraphrases <path>`: a `{"<conversation_id>": "<paraphrase>"}`
//! JSON map (gitignored, lives under /tmp). Conversations missing an entry
//! fall back to a proxy built from their own first chunk — clearly labeled
//! `self-text-proxy` in the output, NOT a paraphrase. A self-text query is
//! an upper bound on hit@6 (near-identical text is easier to recall than a
//! genuine reformulation), not a paraphrase-robustness measurement; treat
//! `self-text-proxy` rows as optimistic relative to hand-authored ones.
//!
//! # Reviewer-fix notes (second pass over the WP-A probe)
//!
//! - `losing_stage` used to fire only on a present->absent transition, which
//!   `rerank`/`fuse_rrf` (reorder, never remove) can never produce — every
//!   loss got misattributed to `final_page`. It's now: the first stage
//!   (in pipeline order) where the target's best rank is missing OR has
//!   reached/passed `LIMIT`, UNLESS the target is present on the actual
//!   final page (that overrides any earlier dip — a later stage can and
//!   does recover rank).
//! - `in_page` used to substring-match the target's raw UUID against the
//!   rendered output, which any OTHER result quoting the query text (e.g. a
//!   conversation ABOUT this exact investigation) can satisfy even when the
//!   target itself is absent. It's now read directly off the `final_page`
//!   stage sample's `target_rank`.
//! - The target query is measured BOTH contaminated (full corpus, as-is)
//!   and decontaminated (excluding conversations whose content contains the
//!   target's own uuid, or whose first-seen timestamp is after the target
//!   conversation's own last timestamp — i.e. conversations that could only
//!   be ABOUT this investigation, not a genuine pre-existing corpus hit).
//!   Decontamination is measured at the brute-force ground-truth layer
//!   (fully under this probe's control); the live `reflect_on_past` pass
//!   still runs over the whole (contaminated) corpus, since excluding
//!   conversations mid-pipeline would mean forking production code — the
//!   contamination receipt below makes that gap explicit rather than
//!   silently laundering a self-referential number.
//! - Brute-force ground truth now runs for every sampled conversation, not
//!   only the target, so an `hnsw_fetch` miss splits into an ANN-recall miss
//!   (brute force says the target WAS within the fetched window's rank
//!   budget; HNSW's approximation missed it) versus a semantic miss (brute
//!   force agrees nothing within that budget was close enough).
//! - The rerank stage now prints the per-term score breakdown (W_USER,
//!   supersedes, mechanic, scaffold, poison, primacy) for the target's best
//!   chunk and every chunk now ranked above it.
//! - `fuse_rrf` prints the target's FTS-side rank alongside its fused rank.
//!
//! # gap-G4-codex-fetch-stage-recall fix notes (third pass)
//!
//! W3-K found 24 of 28 codex misses land in `hnsw_fetch (semantic_miss,
//! outside window)` with no owning item, and flagged every sampled query as
//! a self-text proxy with no paraphrase set ever exercised. This pass:
//! - Replaces the seq=0-`starts_with` harness-boilerplate check with a
//!   running open/close tag-*depth* scan (mirrors
//!   `storage::queries::pick_preview_index`'s logic, private to that
//!   module) — a continuation chunk mid-span (e.g. a plugin-list row, an
//!   AGENTS.md body paragraph) is now skipped along with the chunk that
//!   opened the span, not just chunks that themselves start with a marker.
//! - Adds `classify_semantic_miss` (pure fn, table-tested): every
//!   `hnsw_fetch` semantic_miss is bucketed `harness_polluted` /
//!   `burst_sibling` / `template_sibling` / `contaminated` / `genuine`,
//!   printed as a histogram for both the contaminated and decontaminated
//!   corpus. On the pre-strip live-shaped clone this sample lands
//!   `harness_polluted` dominant for codex, `burst_sibling=0` — burst
//!   collapse (spec 2d) would not move the number, so it was NOT added; no
//!   rerank/trust weight was touched either.
//! - Adds `examples/data/codex_recall_paraphrases.json` (spec 2b) and wires
//!   an 8-gram leak guard (`shares_long_ngram`) that rejects any entry that
//!   copies verbatim text from its own target chunk.
//! - Collapses brute-force ground truth to one row per CONVERSATION (not
//!   per chunk) before ranking/classifying — a rollout holding several
//!   near-duplicate chunks no longer occupies several of the top slots.
//! - `unscoped` now passes `project=Some("all")` explicitly instead of
//!   `None` (W3-S LOW), and every row prints the resolved scope label.
//! - Decontamination for the generalization sweep now excludes, per query,
//!   any conversation whose content contains that query's own text (on top
//!   of the existing global postdate-cutoff set).
//! - Claude's "must not drop" baseline is reported split by templated
//!   (>=20 identical first-120-char siblings) vs non-templated.

use anyhow::Result;
use csr_engine::engine::Engine;
use csr_engine::mcp::stage_probe::{self, StageSample};
use csr_engine::search::rerank::ScoreBreakdown;
use rusqlite::{Connection, OpenFlags};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// The conversation and query WP-A's diagnosis centers on.
const TARGET_CONVERSATION_ID: &str = "codex:019aaaaa-0000-7000-8000-000000000001";
const TARGET_QUERY: &str = "codex blocked revival 2026 iOS deploy blocker where stuck";
const TARGET_PROJECT: &str = "SampleApp";
/// Short form of the target's uuid — what a transcript discussing this exact
/// investigation would quote, and what identifies a contaminating session.
const TARGET_UUID_SUBSTRING: &str = "01a0c0d5";

/// Fixed seed for the deterministic sample selection (blake3 of
/// `"{seed}:{conversation_id}"`, sorted ascending, first N per source).
/// Changing this changes which conversations get sampled — keep it stable
/// across probe runs so before/after tables are directly comparable.
const SAMPLE_SEED: &str = "wpa-recall-sample-v1";

/// `reflect_on_past`'s page size for every probe query — must match what the
/// stage-loss table and hit@N summary below both key off.
const LIMIT: usize = 6;

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

fn sample_key(id: &str) -> [u8; 32] {
    blake3::hash(format!("{SAMPLE_SEED}:{id}").as_bytes()).into()
}

/// One row of the full corpus's embedding table, loaded ONCE and reused for
/// every brute-force ground-truth computation below (the target query plus
/// every sampled conversation) — the corpus doesn't change between queries,
/// only the query vector does.
struct EmbeddingRow {
    conversation_id: String,
    vector: Vec<f32>,
}

fn load_all_embeddings(conn: &Connection) -> Result<Vec<EmbeddingRow>> {
    let mut stmt = conn.prepare(
        "SELECT c.conversation_id, e.embedding FROM chunks c
         JOIN chunk_embeddings e ON e.chunk_id = c.id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(EmbeddingRow {
            conversation_id: row.get::<_, String>(0)?,
            vector: blob_to_vec(&row.get::<_, Vec<u8>>(1)?),
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

struct BruteForce {
    /// Chunks considered (after any exclusion).
    total: usize,
    rank: Option<usize>,
    score: Option<f32>,
}

/// Brute-force exact-cosine ground truth over `rows`, excluding any chunk
/// whose conversation id is in `exclude`. `rank`/`score` are for the target
/// conversation's best (highest-cosine) chunk.
fn brute_force_rank(
    rows: &[EmbeddingRow],
    query_vec: &[f32],
    target_conv: &str,
    exclude: &HashSet<String>,
) -> BruteForce {
    let mut scored: Vec<(f32, &str)> = rows
        .iter()
        .filter(|row| !exclude.contains(&row.conversation_id))
        .map(|row| (cosine(query_vec, &row.vector), row.conversation_id.as_str()))
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let rank = scored.iter().position(|(_, conv)| *conv == target_conv);
    BruteForce {
        total: scored.len(),
        rank,
        score: rank.map(|r| scored[r].0),
    }
}

/// Opus review finding (medium, W3-K second pass): `brute_force_rank` above
/// ranks raw CHUNKS, so one conversation holding several near-duplicate
/// chunks (harness dumps repeated within a rollout are common) can occupy
/// several of the top slots and push the target's rank down without any
/// other CONVERSATION actually outranking it — the real pipeline dedupes by
/// conversation before its page cut, so this collapses the chunk-level score
/// list to the max cosine per conversation first. Used for hit@6 counting
/// and for the classifier's "rows ranked above the target" (never for the
/// `ann_recall_miss` vs `semantic_miss` split against the HNSW fetch window,
/// which stays chunk-level — see `brute_force_rank`'s own doc).
fn collapse_best_per_conversation(rows: &[EmbeddingRow], query_vec: &[f32]) -> Vec<(f32, String)> {
    let mut best: HashMap<&str, f32> = HashMap::new();
    for row in rows {
        let score = cosine(query_vec, &row.vector);
        best.entry(row.conversation_id.as_str())
            .and_modify(|s| {
                if score > *s {
                    *s = score;
                }
            })
            .or_insert(score);
    }
    let mut scored: Vec<(f32, String)> =
        best.into_iter().map(|(k, v)| (v, k.to_string())).collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored
}

/// Conversation-level rank of `target_conv` in a `collapse_best_per_conversation`
/// list, excluding any conversation in `exclude`.
fn brute_force_rank_conv(
    scored: &[(f32, String)],
    target_conv: &str,
    exclude: &HashSet<String>,
) -> BruteForce {
    let filtered: Vec<&(f32, String)> = scored
        .iter()
        .filter(|(_, conv)| !exclude.contains(conv))
        .collect();
    let rank = filtered.iter().position(|(_, conv)| conv == target_conv);
    BruteForce {
        total: filtered.len(),
        rank,
        score: rank.map(|r| filtered[r].0),
    }
}

/// Mirrors `storage::queries::{codex_spawn_time_from_id, within_burst_window,
/// CODEX_BURST_WINDOW_SECS}` (private to that module). Best-effort spawn
/// time embedded in a UUIDv7-shaped Codex conversation id's first 48 bits
/// (big-endian Unix milliseconds), `None` for a legacy/malformed id.
fn codex_spawn_time_from_id(conversation_id: &str) -> Option<String> {
    let uuid_part = conversation_id.strip_prefix("codex:")?;
    let hex: String = uuid_part.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    if hex.as_bytes()[12] != b'7' {
        return None;
    }
    let ms = u64::from_str_radix(&hex[..12], 16).ok()?;
    let secs = (ms / 1000) as i64;
    let nanos = ((ms % 1000) * 1_000_000) as u32;
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, nanos).map(|dt| dt.to_rfc3339())
}

const CODEX_BURST_WINDOW_SECS: i64 = 10;

fn burst_key_ts(conversation_id: &str, first_chunk_ts: &str) -> String {
    codex_spawn_time_from_id(conversation_id).unwrap_or_else(|| first_chunk_ts.to_string())
}

fn within_burst_window(head_ts: &str, candidate_ts: &str) -> bool {
    match (
        head_ts.parse::<chrono::DateTime<chrono::Utc>>(),
        candidate_ts.parse::<chrono::DateTime<chrono::Utc>>(),
    ) {
        (Ok(head), Ok(candidate)) => {
            (head - candidate).num_seconds().abs() <= CODEX_BURST_WINDOW_SECS
        }
        _ => head_ts.get(..19) == candidate_ts.get(..19),
    }
}

/// Classification of an `hnsw_fetch (semantic_miss, ...)` — why the target
/// conversation didn't rank in the top window even under brute-force ground
/// truth. Spec 2(a), hardened per the Opus review's harness-first-priority
/// finding (high): a query built from harness boilerplate must never be
/// scored as a "same-text sibling" problem, since fixing THAT class means
/// search-side burst collapse, which does nothing for a query that is
/// boilerplate to begin with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum MissClass {
    /// The query text itself (or the target's own proxy chunk) sits inside
    /// an open Codex harness tag span — the query was never a real user
    /// message to begin with.
    HarnessPolluted,
    /// A conversation ranked above the target has near-identical text AND
    /// spawned within `CODEX_BURST_WINDOW_SECS` of it — a parallel
    /// `codex_exec` burst sibling. Candidate for search-side burst collapse.
    BurstSibling,
    /// A conversation ranked above the target has near-identical text but
    /// was NOT spawned within the burst window (e.g. the same template
    /// reused weeks apart) — burst collapse would not fix this.
    TemplateSibling,
    /// A conversation ranked above the target is in the contamination set
    /// (postdates the global cutoff, or quotes the query text).
    Contaminated,
    /// None of the above explain the miss — a genuine embedding-recall gap.
    Genuine,
}

impl std::fmt::Display for MissClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            MissClass::HarnessPolluted => "harness_polluted",
            MissClass::BurstSibling => "burst_sibling",
            MissClass::TemplateSibling => "template_sibling",
            MissClass::Contaminated => "contaminated",
            MissClass::Genuine => "genuine",
        };
        f.write_str(s)
    }
}

/// Cosine similarity at/above this counts two conversations' proxy content as
/// "the same text" for sibling classification — high enough that ordinary
/// topical similarity can't cross it, only a near-duplicate can.
const SIBLING_COSINE_THRESHOLD: f32 = 0.97;

struct MissTarget {
    burst_key_ts: String,
    harness_polluted: bool,
}

struct SiblingRow {
    cosine: f32,
    burst_key_ts: String,
    identical_text: bool,
    contaminated: bool,
}

/// Pure classifier: given the target and the brute-force conversation rows
/// ranked above it (already deduped per conversation — see
/// `collapse_best_per_conversation`), decide which of [`MissClass`]'s five
/// buckets explains the miss. Priority order matches the Opus review's
/// harness-first finding: a harness-polluted query is classified as such
/// regardless of what's ranked above it (checking siblings first would
/// misclassify it as `same_text_sibling`, which points at the wrong fix).
fn classify_semantic_miss(target: &MissTarget, rows_above: &[SiblingRow]) -> MissClass {
    if target.harness_polluted {
        return MissClass::HarnessPolluted;
    }
    let mut has_burst = false;
    let mut has_template = false;
    let mut has_contaminated = false;
    for row in rows_above {
        let is_sibling = row.cosine >= SIBLING_COSINE_THRESHOLD || row.identical_text;
        if is_sibling {
            if within_burst_window(&target.burst_key_ts, &row.burst_key_ts) {
                has_burst = true;
            } else {
                has_template = true;
            }
        }
        if row.contaminated {
            has_contaminated = true;
        }
    }
    if has_burst {
        MissClass::BurstSibling
    } else if has_template {
        MissClass::TemplateSibling
    } else if has_contaminated {
        MissClass::Contaminated
    } else {
        MissClass::Genuine
    }
}

#[cfg(test)]
mod classify_tests {
    use super::*;

    fn target(harness_polluted: bool) -> MissTarget {
        MissTarget {
            burst_key_ts: "2026-09-20T10:00:00Z".to_string(),
            harness_polluted,
        }
    }

    #[test]
    fn harness_polluted_wins_even_with_a_sibling_above() {
        let t = target(true);
        let rows = [SiblingRow {
            cosine: 0.99,
            burst_key_ts: "2026-09-20T10:00:05Z".to_string(),
            identical_text: true,
            contaminated: false,
        }];
        assert_eq!(
            classify_semantic_miss(&t, &rows),
            MissClass::HarnessPolluted
        );
    }

    #[test]
    fn burst_sibling_within_window() {
        let t = target(false);
        let rows = [SiblingRow {
            cosine: 0.98,
            burst_key_ts: "2026-09-20T10:00:08Z".to_string(), // 8s away, within 10s window
            identical_text: false,
            contaminated: false,
        }];
        assert_eq!(classify_semantic_miss(&t, &rows), MissClass::BurstSibling);
    }

    #[test]
    fn template_sibling_outside_window() {
        let t = target(false);
        let rows = [SiblingRow {
            cosine: 0.0,
            burst_key_ts: "2026-08-05T10:00:00Z".to_string(), // days away
            identical_text: true,
            contaminated: false,
        }];
        assert_eq!(
            classify_semantic_miss(&t, &rows),
            MissClass::TemplateSibling
        );
    }

    #[test]
    fn contaminated_when_no_sibling_but_a_contaminated_row_above() {
        let t = target(false);
        let rows = [SiblingRow {
            cosine: 0.4,
            burst_key_ts: "2026-09-22T15:00:00Z".to_string(),
            identical_text: false,
            contaminated: true,
        }];
        assert_eq!(classify_semantic_miss(&t, &rows), MissClass::Contaminated);
    }

    #[test]
    fn genuine_when_nothing_above_explains_it() {
        let t = target(false);
        let rows = [SiblingRow {
            cosine: 0.3,
            burst_key_ts: "2026-09-01T00:00:00Z".to_string(),
            identical_text: false,
            contaminated: false,
        }];
        assert_eq!(classify_semantic_miss(&t, &rows), MissClass::Genuine);
    }

    #[test]
    fn genuine_when_no_rows_above() {
        let t = target(false);
        assert_eq!(classify_semantic_miss(&t, &[]), MissClass::Genuine);
    }

    #[test]
    fn burst_beats_template_when_both_present() {
        let t = target(false);
        let rows = [
            SiblingRow {
                cosine: 0.0,
                burst_key_ts: "2026-08-05T10:00:00Z".to_string(),
                identical_text: true,
                contaminated: false,
            },
            SiblingRow {
                cosine: 0.98,
                burst_key_ts: "2026-09-20T10:00:02Z".to_string(),
                identical_text: false,
                contaminated: false,
            },
        ];
        assert_eq!(classify_semantic_miss(&t, &rows), MissClass::BurstSibling);
    }
}

/// Conversations that could only be ABOUT the target investigation, not a
/// genuine pre-existing corpus hit: their content quotes the target's own
/// uuid, or they first appear after the target conversation's own last
/// timestamp (so they postdate it and can plausibly only be discussing it).
struct Contamination {
    /// `(conversation_id, first_timestamp, matched_substring)`.
    rows: Vec<(String, String, bool)>,
    conv_ids: HashSet<String>,
}

fn detect_contamination(conn: &Connection, target_conv: &str) -> Result<Contamination> {
    let cutoff: String = conn.query_row(
        "SELECT MAX(timestamp) FROM chunks WHERE conversation_id = ?1",
        [target_conv],
        |row| row.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT conversation_id, MIN(timestamp) AS first_ts,
                MAX(CASE WHEN content LIKE '%' || ?1 || '%' THEN 1 ELSE 0 END) AS matched
         FROM chunks
         WHERE conversation_id != ?2
         GROUP BY conversation_id
         HAVING matched = 1 OR first_ts > ?3
         ORDER BY first_ts ASC",
    )?;
    let rows: Vec<(String, String, bool)> = stmt
        .query_map((TARGET_UUID_SUBSTRING, target_conv, &cutoff), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? == 1,
            ))
        })?
        .collect::<std::result::Result<_, _>>()?;
    let conv_ids = rows.iter().map(|(id, _, _)| id.clone()).collect();
    Ok(Contamination { rows, conv_ids })
}

/// Sessions produced by this wave-2 investigation itself, which quote its
/// probe queries verbatim (recorded in `docs/`/agent journals: sessions
/// `51a3c211`, `432ec120` and their workflow sidechains — every one first
/// seen after this cutoff). Unlike [`detect_contamination`] (which is
/// target-specific: it also matches on a target's own uuid substring), this
/// is the CHEAP, target-independent half of the same rule — "exclude
/// conversations whose first timestamp is after 2026-09-22T14:00Z" — usable
/// for every sampled conversation in the generalization sweep without a
/// per-conversation contamination scan.
const GLOBAL_CONTAMINATION_CUTOFF: &str = "2026-09-22T14:00:00Z";

/// Conversation ids whose earliest chunk postdates
/// [`GLOBAL_CONTAMINATION_CUTOFF`] — the target-independent half of
/// contamination, computed once and reused as the `exclude` set for every
/// sampled conversation's decontaminated brute-force rank.
fn global_contamination_conv_ids(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT conversation_id FROM chunks
         GROUP BY conversation_id
         HAVING MIN(timestamp) > ?1",
    )?;
    let ids = stmt
        .query_map([GLOBAL_CONTAMINATION_CUTOFF], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<HashSet<_>, _>>()?;
    Ok(ids)
}

/// Opus review finding (medium, W3-K second pass): the shared-context rule
/// is "postdates the cutoff OR quotes the query text" — both halves.
/// [`global_contamination_conv_ids`] is the cheap, query-independent cutoff
/// half; this is the per-query content-match half, run once per sampled
/// query on top of it. Matches on a normalized 80-char slice of the query
/// text (not the whole query), so a long query still catches a conversation
/// that quotes only a fragment of it.
fn query_contamination_conv_ids(
    conn: &Connection,
    query_text: &str,
    exclude_self: &str,
) -> Result<HashSet<String>> {
    let slice: String = query_text.chars().take(80).collect();
    if slice.trim().chars().count() < 8 {
        // Too short to be a meaningful substring match — a short slice would
        // hit nearly every conversation and over-exclude rather than never
        // excluding, so skip instead.
        return Ok(HashSet::new());
    }
    let mut stmt = conn.prepare(
        "SELECT DISTINCT conversation_id FROM chunks
         WHERE conversation_id != ?2 AND content LIKE '%' || ?1 || '%'",
    )?;
    let ids = stmt
        .query_map((&slice, exclude_self), |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<HashSet<_>, _>>()?;
    Ok(ids)
}

/// Per-conversation `(first_seen_timestamp, content_norm)` across BOTH main
/// sources (`codex_rollout`, `conversation`) — the corpus-wide metadata the
/// classifier needs for every sibling ranked above a miss, not just the
/// sampled conversations. `content_norm` mirrors `SampleConv::content_norm`
/// (the depth-aware first-real-chunk proxy), so an identical-text sibling
/// check is comparing the same normalization on both sides.
fn build_conv_meta(conn: &Connection) -> Result<HashMap<String, (String, String)>> {
    let mut first_ts: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT conversation_id, MIN(timestamp) FROM chunks GROUP BY conversation_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (conv, ts) = row?;
            first_ts.insert(conv, ts);
        }
    }
    let mut meta: HashMap<String, (String, String)> = HashMap::new();
    for source in ["codex_rollout", "conversation"] {
        for (conv, _project, content, _harness_polluted) in first_chunks_for_source(conn, source)? {
            let ts = first_ts.get(&conv).cloned().unwrap_or_default();
            meta.insert(conv, (ts, self_text_proxy(&content)));
        }
    }
    Ok(meta)
}

/// Mirrors `import::codex_rollout::CODEX_HARNESS_MARKERS` (private to that
/// module): Codex harness-injected boilerplate (plugin recommendations,
/// AGENTS.md instructions, environment-context dumps) that can occupy a
/// conversation's earliest chunk(s) entirely. Discovered while building this
/// probe's sample: for several sampled `codex_rollout` conversations, the
/// literal seq=0 chunk was `<recommended_plugins>\nHere is a list of
/// plugins...` — querying with THAT text is not testing recall at all, it's
/// testing whether the corpus has other harness dumps. The harness-strip fix
/// (`canonical_message`, ba8f18c) removes these markers from messages that
/// MIX boilerplate with real content, but a message that is ENTIRELY
/// boilerplate still becomes its own, real, boilerplate-only chunk — this
/// list exists so query-text SELECTION (not import) skips those chunks.
const CODEX_HARNESS_MARKERS: &[&str] = &[
    "<recommended_plugins>",
    "# AGENTS.md instructions",
    "<environment_context>",
    "<user_instructions>",
    "<permissions instructions>",
    "<codex_internal_context>",
    "<skills_instructions>",
];

/// Mirrors `storage::queries::CODEX_HARNESS_TAG_PAIRS` (private to that
/// module, so this probe — a separate `examples/` binary that only sees the
/// crate's public API — cannot import it directly). Open/close tag pairs for
/// the harness blocks `CODEX_HARNESS_MARKERS` above can open.
const CODEX_HARNESS_TAG_PAIRS: &[(&str, &str)] = &[
    ("<recommended_plugins>", "</recommended_plugins>"),
    ("<INSTRUCTIONS>", "</INSTRUCTIONS>"),
    ("<environment_context>", "</environment_context>"),
    ("<user_instructions>", "</user_instructions>"),
    ("<permissions instructions>", "</permissions instructions>"),
    ("<codex_internal_context>", "</codex_internal_context>"),
    ("<skills_instructions>", "</skills_instructions>"),
];

/// Mirrors `storage::queries::harness_tag_depth_delta` (private). Net
/// harness-tag depth contributed by one chunk: total opens minus total
/// closes across every pair in `CODEX_HARNESS_TAG_PAIRS`.
fn harness_tag_depth_delta(content: &str) -> i32 {
    CODEX_HARNESS_TAG_PAIRS
        .iter()
        .map(|(open, close)| {
            content.matches(open).count() as i32 - content.matches(close).count() as i32
        })
        .sum()
}

/// Opus review finding (high, W3-K second pass): `starts_with(marker)` only
/// catches a chunk that OPENS a harness block, not a continuation chunk
/// sitting in the middle of one — real examples include a plugin-list
/// continuation chunk starting `"Monday.com (monday-com@openai-curated-...)"`
/// and an AGENTS.md body chunk starting `"- Setup: \`npm run setup\`..."`,
/// neither of which starts with any marker yet both are harness boilerplate.
/// This tracks a running open/close *depth* across `chunks` (already ordered
/// by seq/rowid) the same way `storage::queries::pick_preview_index` does,
/// so a still-open span is skipped in full, not just its opening chunk.
/// Returns the index of the first real (non-boilerplate, depth==0) chunk, or
/// `None` if every chunk in the window is inside a harness span (an
/// entirely-boilerplate conversation).
fn first_real_chunk_index(chunks: &[&str]) -> Option<usize> {
    let mut depth: i32 = 0;
    for (i, content) in chunks.iter().enumerate() {
        let trimmed = content.trim_start();
        let opens_here = CODEX_HARNESS_MARKERS
            .iter()
            .any(|marker| trimmed.starts_with(marker));
        if depth > 0 || opens_here {
            depth = (depth + harness_tag_depth_delta(content)).max(0);
            continue;
        }
        return Some(i);
    }
    None
}

/// One row per `conversation_id` for `source`: its project name, its
/// earliest NON-boilerplate chunk's content (depth-tracked span skip, see
/// [`first_real_chunk_index`]) used as the self-text proxy query when no
/// hand-authored paraphrase is supplied, and whether the conversation is
/// entirely harness boilerplate (no real chunk found in the fetched window —
/// the query text itself would then be harness-polluted, not a genuine
/// sample). Falls back to the literal first chunk in that case, so the
/// sample never silently drops a conversation.
fn first_chunks_for_source(
    conn: &Connection,
    source: &str,
) -> Result<Vec<(String, String, String, bool)>> {
    let mut stmt = conn.prepare(
        "SELECT conversation_id, project_name, content FROM chunks
         WHERE source = ?1
         ORDER BY conversation_id ASC, seq ASC, rowid ASC",
    )?;
    let rows = stmt.query_map([source], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let all: Vec<(String, String, String)> = rows.collect::<std::result::Result<Vec<_>, _>>()?;

    // Group by conversation, preserving seq/rowid order within each group.
    let mut by_conv: Vec<(String, String, Vec<String>)> = Vec::new(); // (conv, project, [content...])
    for (conv, project, content) in all {
        match by_conv.last_mut() {
            Some((last_conv, _, contents)) if *last_conv == conv => contents.push(content),
            _ => by_conv.push((conv, project, vec![content])),
        }
    }

    let mut out: Vec<(String, String, String, bool)> = Vec::with_capacity(by_conv.len());
    for (conv, project, contents) in by_conv {
        let refs: Vec<&str> = contents.iter().map(String::as_str).collect();
        match first_real_chunk_index(&refs) {
            Some(idx) => out.push((conv, project, contents[idx].clone(), false)),
            None => out.push((conv, project, contents[0].clone(), true)),
        }
    }
    Ok(out)
}

/// A proxy query text built from a conversation's own opening content: strip
/// obvious tool/JSON boilerplate characters, collapse whitespace, take the
/// first ~220 chars. Deliberately NOT a paraphrase.
fn self_text_proxy(content: &str) -> String {
    let cleaned: String = content
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(220).collect()
}

struct SampleConv {
    conversation_id: String,
    project_name: String,
    query_text: String,
    query_source: &'static str,
    /// True when [`first_real_chunk_index`] found no real chunk at all for
    /// this conversation — its self-text-proxy query is itself harness
    /// boilerplate (query-side harness pollution, Opus review high finding).
    /// Always `false` for a hand-authored `paraphrase` row.
    harness_polluted: bool,
    /// Normalized proxy of the conversation's own first real chunk (built
    /// the same way regardless of `query_source`) — used by the classifier
    /// to detect a sibling whose own first-chunk text is (near-)identical to
    /// this conversation's, independent of which query text was sent.
    content_norm: String,
}

/// Spec 2(b) guard: a paraphrase must not leak verbatim text from the
/// target's own chunk — true when `a` and `b` share an identical run of `n`
/// consecutive whitespace-separated tokens.
fn shares_long_ngram(a: &str, b: &str, n: usize) -> bool {
    let a_tokens: Vec<&str> = a.split_whitespace().collect();
    if a_tokens.len() < n {
        return false;
    }
    let b_tokens: Vec<&str> = b.split_whitespace().collect();
    if b_tokens.len() < n {
        return false;
    }
    let b_ngrams: HashSet<&[&str]> = b_tokens.windows(n).collect();
    a_tokens.windows(n).any(|w| b_ngrams.contains(w))
}

fn build_sample(
    conn: &Connection,
    source: &str,
    n: usize,
    paraphrases: &HashMap<String, String>,
) -> Result<Vec<SampleConv>> {
    let mut rows = first_chunks_for_source(conn, source)?;
    rows.sort_by_key(|a| sample_key(&a.0));
    Ok(rows
        .into_iter()
        .take(n)
        .map(
            |(conversation_id, project_name, content, harness_polluted)| {
                let content_norm = self_text_proxy(&content);
                let paraphrase = paraphrases.get(&conversation_id).filter(|p| {
                    if shares_long_ngram(p, &content, 8) {
                        eprintln!(
                            "WARN: paraphrase for {conversation_id} shares an 8-gram with its own \
                         chunk — rejected, falling back to self-text-proxy"
                        );
                        false
                    } else {
                        true
                    }
                });
                if let Some(paraphrase) = paraphrase {
                    SampleConv {
                        conversation_id,
                        project_name,
                        query_text: paraphrase.clone(),
                        query_source: "paraphrase",
                        harness_polluted: false,
                        content_norm,
                    }
                } else {
                    SampleConv {
                        conversation_id,
                        project_name,
                        query_text: content_norm.clone(),
                        query_source: "self-text-proxy",
                        harness_polluted,
                        content_norm,
                    }
                }
            },
        )
        .collect())
}

fn fmt_breakdown(b: &ScoreBreakdown, primacy: f32) -> String {
    format!(
        "cos={:.3} user={:+.2} supersedes={:+.2} mechanic={:+.2} scaffold={:+.2} poison={:+.2} primacy={:+.2} total={:.3}",
        b.cosine, b.user_boost, b.supersedes_boost, b.mechanic_penalty, b.scaffold_penalty,
        b.poison_penalty, primacy, b.total + primacy,
    )
}

fn print_stage_table(label: &str, samples: &[StageSample]) {
    println!("  [{label}]");
    for sample in samples {
        let rank = sample
            .target_rank
            .map(|r| (r + 1).to_string())
            .unwrap_or_else(|| "LOST".to_string());
        let score = sample
            .target_score
            .map(|s| format!("{s:.4}"))
            .unwrap_or_else(|| "-".to_string());
        let fts_rank = sample
            .target_fts_rank
            .map(|r| format!(" fts_rank={}", r + 1))
            .unwrap_or_default();
        println!(
            "    {:<16} total={:<5} rank={:<5} score={}{}",
            sample.stage, sample.total_candidates, rank, score, fts_rank
        );
        if !sample.rerank_breakdown.is_empty() {
            for row in &sample.rerank_breakdown {
                let tag = if row.is_target {
                    "TARGET  "
                } else {
                    "displace"
                };
                println!(
                    "        {tag} {:<24} {}",
                    &row.chunk_id[..row.chunk_id.len().min(24)],
                    fmt_breakdown(&row.breakdown, row.primacy_boost)
                );
            }
        }
    }
}

/// The stage at which the target's best rank stopped being on the page.
///
/// Definition (fix for the reviewer-caught bug in the previous version): the
/// FIRST stage, in pipeline order, whose target rank is missing or has
/// reached/passed `limit` — UNLESS the target is present on the actual final
/// page, in which case a later stage recovered it and there is no losing
/// stage at all. `rerank`/`fuse_rrf` reorder candidates without removing any,
/// so treating "present but ranked >= limit" as already off the page (at
/// that stage) is what lets those stages ever get convicted; the prior
/// present->absent-only definition could never attribute a loss to them.
fn losing_stage(samples: &[StageSample], limit: usize) -> String {
    if samples.is_empty() {
        return "no stages recorded (probe scope errored before any checkpoint)".to_string();
    }
    if samples.last().is_some_and(|s| s.target_rank.is_some()) {
        return "none (present on final page)".to_string();
    }
    for sample in samples {
        let lost = match sample.target_rank {
            None => true,
            Some(rank) => rank >= limit,
        };
        if lost {
            return sample.stage.to_string();
        }
    }
    // Every recorded stage had rank < limit yet the target is absent from
    // the final page — should be unreachable (final_page's own vec is
    // already limit-truncated, so a rank < limit there is a hit), but don't
    // silently mislabel if some future stage violates that invariant.
    "final_page".to_string()
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut db = PathBuf::from("/tmp/csr-wpa/csr-engine.db");
    let mut projects_dir = dirs::home_dir().expect("home dir").join(".claude/projects");
    let mut sample_n = 20usize;
    let mut paraphrases_path: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--db" => db = PathBuf::from(args.next().expect("--db needs a value")),
            "--projects-dir" => {
                projects_dir = PathBuf::from(args.next().expect("--projects-dir needs a value"))
            }
            "--sample" => {
                sample_n = args
                    .next()
                    .expect("--sample needs a value")
                    .parse()
                    .expect("--sample must be a number")
            }
            "--paraphrases" => {
                paraphrases_path = Some(PathBuf::from(
                    args.next().expect("--paraphrases needs a value"),
                ))
            }
            other => anyhow::bail!("unrecognized argument: {other}"),
        }
    }

    anyhow::ensure!(
        db != dirs::home_dir()
            .expect("home dir")
            .join(".claude-self-reflect/csr-engine.db"),
        "refusing to run against the live database — pass a /tmp clone via --db"
    );

    let paraphrases: HashMap<String, String> = match &paraphrases_path {
        Some(path) => {
            let raw = std::fs::read_to_string(path)?;
            serde_json::from_str(&raw)?
        }
        None => HashMap::new(),
    };

    println!("=== WP-A codex recall stage probe ===");
    println!("db: {}", db.display());
    println!(
        "paraphrases: {}",
        paraphrases_path
            .as_ref()
            .map(|p| format!("{} ({} entries)", p.display(), paraphrases.len()))
            .unwrap_or_else(|| "none supplied — all rows use self-text-proxy".to_string())
    );

    let engine = Engine::new(&db, &projects_dir)?;
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;

    println!("\nloading full embedding corpus for brute-force ground truth...");
    let all_rows = load_all_embeddings(&conn)?;
    println!("  {} chunk embeddings loaded", all_rows.len());

    // ── Part 1: the single target query, project-scoped and unscoped ──
    println!(
        "\n--- target: {TARGET_CONVERSATION_ID} | query: {TARGET_QUERY:?} | project: {TARGET_PROJECT}/all ---"
    );

    {
        let target_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM chunks WHERE conversation_id = ?1",
            [TARGET_CONVERSATION_ID],
            |row| row.get(0),
        )?;
        println!("target chunk count in this db: {target_count}");
    }

    let contamination = detect_contamination(&conn, TARGET_CONVERSATION_ID)?;
    println!(
        "\ncontamination receipt: {} conversation(s) quote the target's uuid or postdate its own last timestamp",
        contamination.rows.len()
    );
    for (conv, first_ts, matched_substring) in &contamination.rows {
        println!("  {conv:<50} first_seen={first_ts} quotes_uuid={matched_substring}");
    }
    println!(
        "note: `reflect_on_past` below still runs over the FULL (contaminated) corpus — \
         excluding these conversations mid-pipeline would mean forking production code. \
         The brute-force ground truth immediately below is measured BOTH ways."
    );

    // Ground truth (stage b): brute-force cosine over EVERY stored chunk
    // embedding, compared against the HNSW-served hnsw_fetch stage below —
    // this is the only stage that does NOT come from the real pipeline
    // (there is no public brute-force entry point on `SearchEngine`), so it
    // is computed independently here rather than instrumented in-process.
    let query_vec = engine.embeddings().embed_single(TARGET_QUERY)?;
    {
        let contaminated = brute_force_rank(
            &all_rows,
            &query_vec,
            TARGET_CONVERSATION_ID,
            &HashSet::new(),
        );
        let decontaminated = brute_force_rank(
            &all_rows,
            &query_vec,
            TARGET_CONVERSATION_ID,
            &contamination.conv_ids,
        );
        for (label, bf) in [
            ("contaminated", &contaminated),
            ("decontaminated", &decontaminated),
        ] {
            println!(
                "  [brute_force_ground_truth:{label}] total={} target_rank={} target_score={}",
                bf.total,
                bf.rank
                    .map(|r| (r + 1).to_string())
                    .unwrap_or_else(|| "LOST".to_string()),
                bf.score
                    .map(|s| format!("{s:.4}"))
                    .unwrap_or_else(|| "-".to_string()),
            );
        }
    }

    for (label, project) in [
        ("project=SampleApp", Some(TARGET_PROJECT)),
        ("project=all", Some("all")),
    ] {
        let (result, samples) = stage_probe::scope(TARGET_CONVERSATION_ID, || {
            csr_engine::mcp::tools::reflect_on_past(
                engine.storage(),
                engine.embeddings(),
                engine.search(),
                TARGET_QUERY,
                LIMIT,
                0.1,
                project,
            )
        })
        .await;
        match result {
            Ok(_out) => {
                // `in_page` is read directly off the (limit-truncated)
                // final_page stage sample, NOT an output substring — any OTHER
                // result quoting the query text (a conversation ABOUT this
                // investigation) could previously satisfy a substring check
                // even with the target itself absent.
                let in_page = samples
                    .last()
                    .is_some_and(|sample| sample.target_rank.is_some());
                println!("  reflect_on_past({label}): target in final page={in_page}");
            }
            Err(error) => println!("  reflect_on_past({label}): ERROR {error}"),
        }
        print_stage_table(label, &samples);
        println!("  losing stage: {}", losing_stage(&samples, LIMIT));
    }

    // ── Part 2: generalization — >=N codex + >=N Claude conversations ──
    let codex_sample = build_sample(&conn, "codex_rollout", sample_n, &paraphrases)?;
    let claude_sample = build_sample(&conn, "conversation", sample_n, &paraphrases)?;
    let global_contamination = global_contamination_conv_ids(&conn)?;
    println!(
        "\nglobal contamination set (first_seen > {GLOBAL_CONTAMINATION_CUTOFF}): {} conversation(s)",
        global_contamination.len()
    );
    let conv_meta = build_conv_meta(&conn)?;
    println!(
        "conversation metadata (first_ts + content_norm) built for {} conversations",
        conv_meta.len()
    );

    // Opus review finding (low): the claude baseline that "must not drop" is
    // itself dominated by templated first chunks (narrator/summary
    // boilerplate, `<local-command-caveat>` echoes). Group by a normalized
    // first-120-char prefix; 20+ identical members marks the group
    // templated — reported per conversation below so the claude hit@6 split
    // is visible, and so paraphrase authoring can prefer non-templated ids.
    let mut claude_template_groups: HashMap<String, usize> = HashMap::new();
    for (_, (_, content_norm)) in conv_meta.iter().filter(|(id, _)| !id.starts_with("codex:")) {
        let prefix: String = content_norm.chars().take(120).collect();
        *claude_template_groups.entry(prefix).or_insert(0) += 1;
    }
    let is_templated = |content_norm: &str| -> bool {
        let prefix: String = content_norm.chars().take(120).collect();
        claude_template_groups.get(&prefix).copied().unwrap_or(0) >= 20
    };
    println!(
        "\n--- generalization sample: {} codex, {} claude (seed={SAMPLE_SEED:?}) ---",
        codex_sample.len(),
        claude_sample.len()
    );
    println!(
        "codex ids: {:?}",
        codex_sample
            .iter()
            .map(|c| &c.conversation_id)
            .collect::<Vec<_>>()
    );
    println!(
        "claude ids: {:?}",
        claude_sample
            .iter()
            .map(|c| &c.conversation_id)
            .collect::<Vec<_>>()
    );

    let mut hit_counts: HashMap<&str, (usize, usize)> = HashMap::new(); // source -> (hits, total)
    let mut miss_stage_counts: HashMap<(&str, String), usize> = HashMap::new();
    // Decontaminated hit@6, brute-force ground truth (same methodology and
    // same caveat as the target's own decontaminated number above: measured
    // outside the live pipeline, which has no exclusion parameter to fork
    // production code for). `None` when the sampled conversation is itself
    // in the global contamination set — its own recall can't be scored
    // meaningfully once its content is what "decontaminated" would exclude.
    let mut decontam_hit_counts: HashMap<&str, (usize, usize)> = HashMap::new();
    /// Opus review finding (high, W3-K EVAL GATE item 3): the brute-force
    /// number above is ground truth over raw embeddings, never a number the
    /// real pipeline produces. This runs the REAL `reflect_on_past` (scoped
    /// to the conversation's own project, the realistic call shape) at an
    /// overfetched limit, strips global-contamination conversations from the
    /// `final_page` stage's full ranked list, then re-counts hit@6 over the
    /// filtered survivors — a genuine pipeline decontaminated hit@6, not a
    /// brute-force proxy. `None` for the same reason as the brute-force
    /// number: the conversation is itself in the global contamination set,
    /// or the scoped call errored.
    const PIPELINE_DECONTAM_OVERFETCH: usize = LIMIT * 5;
    let mut pipeline_decontam_hit_counts: HashMap<&str, (usize, usize)> = HashMap::new();
    // Spec 2(a): histogram of `hnsw_fetch (semantic_miss, ...)` classes, kept
    // separately for the contaminated (full corpus) and decontaminated
    // (global cutoff + per-query content match) runs — a class that
    // disappears going from contaminated to decontaminated tells us the
    // contamination set was crowding the target out; one that persists is a
    // real recall problem the decontamination can't paper over.
    let mut miss_class_contam: HashMap<(&str, MissClass), usize> = HashMap::new();
    let mut miss_class_decontam: HashMap<(&str, MissClass), usize> = HashMap::new();
    // Claude "must not drop" baseline split by templated vs non-templated
    // first chunk (Opus review low finding).
    let mut claude_hit_by_template: HashMap<bool, (usize, usize)> = HashMap::new(); // is_templated -> (hits, total)

    for (source_label, sample) in [("codex", &codex_sample), ("claude", &claude_sample)] {
        for conv in sample {
            let conv_query_vec = engine.embeddings().embed_single(&conv.query_text).ok();

            // Opus review finding (medium): decontamination is "postdates the
            // cutoff OR quotes the query text" — both halves, per query.
            let query_contam =
                query_contamination_conv_ids(&conn, &conv.query_text, &conv.conversation_id)?;
            if !query_contam.is_empty() {
                println!(
                    "  {source_label:<6} {} query-contamination: {} conversation(s) additionally excluded (quote this query)",
                    conv.conversation_id,
                    query_contam.len()
                );
            }
            let decontam_exclude: HashSet<String> =
                global_contamination.union(&query_contam).cloned().collect();

            // Opus review finding (medium): rank/dedupe by CONVERSATION, not
            // chunk, before deciding hit@6 or classifying a miss — a
            // conversation holding several near-duplicate chunks otherwise
            // gets counted several times against the target.
            let scored = conv_query_vec
                .as_deref()
                .map(|qv| collapse_best_per_conversation(&all_rows, qv));

            let decontam_hit = if global_contamination.contains(&conv.conversation_id) {
                None
            } else {
                scored.as_ref().map(|s| {
                    let bf = brute_force_rank_conv(s, &conv.conversation_id, &decontam_exclude);
                    bf.rank.is_some_and(|r| r < LIMIT)
                })
            };
            match decontam_hit {
                Some(hit) => {
                    let entry = decontam_hit_counts.entry(source_label).or_insert((0, 0));
                    entry.1 += 1;
                    if hit {
                        entry.0 += 1;
                    }
                    println!(
                        "  {source_label:<6} {} decontam_hit@6(brute_force, per-conversation)={hit}",
                        conv.conversation_id
                    );
                }
                None => println!(
                    "  {source_label:<6} {} decontam_hit@6(brute_force)=n/a (self-contaminated)",
                    conv.conversation_id
                ),
            }

            let pipeline_decontam_hit = if global_contamination.contains(&conv.conversation_id) {
                None
            } else {
                let (result, overfetch_samples) = stage_probe::scope(&conv.conversation_id, || {
                    csr_engine::mcp::tools::reflect_on_past(
                        engine.storage(),
                        engine.embeddings(),
                        engine.search(),
                        &conv.query_text,
                        PIPELINE_DECONTAM_OVERFETCH,
                        0.1,
                        Some(conv.project_name.as_str()),
                    )
                })
                .await;
                match result {
                    Ok(_) => overfetch_samples
                        .iter()
                        .find(|s| s.stage == "final_page")
                        .map(|final_page| {
                            final_page
                                .ranked_conversations
                                .iter()
                                .filter(|c| !global_contamination.contains(*c))
                                .take(LIMIT)
                                .any(|c| c == &conv.conversation_id)
                        }),
                    Err(_) => None,
                }
            };
            match pipeline_decontam_hit {
                Some(hit) => {
                    let entry = pipeline_decontam_hit_counts
                        .entry(source_label)
                        .or_insert((0, 0));
                    entry.1 += 1;
                    if hit {
                        entry.0 += 1;
                    }
                    println!(
                        "  {source_label:<6} {} decontam_hit@6(pipeline, overfetch={PIPELINE_DECONTAM_OVERFETCH})={hit}",
                        conv.conversation_id
                    );
                }
                None => println!(
                    "  {source_label:<6} {} decontam_hit@6(pipeline)=n/a (self-contaminated or error)",
                    conv.conversation_id
                ),
            }

            // Opus review finding (medium, W3-S LOW): pass `Some("all")`, not
            // `None`, for the "unscoped" arm — `None` resolves through
            // `SearchProjectScope::resolve_with`'s env-derived
            // `default_scope_project` fallback (`MCP_CLIENT_CWD` /
            // `CLAUDE_PROJECT_DIR`), which silently re-scopes "unscoped" runs
            // to whatever project this Bash session happens to be in.
            // `resolve_with` maps a case-insensitive `"all"` request straight
            // to `scope_label = "all"` unconditionally, bypassing the env
            // fallback entirely — printed below so the receipt proves it.
            for (scope_label, project, resolved_scope_label) in [
                (
                    "scoped",
                    Some(conv.project_name.as_str()),
                    conv.project_name.as_str(),
                ),
                ("unscoped", Some("all"), "all"),
            ] {
                let (result, samples) = stage_probe::scope(&conv.conversation_id, || {
                    csr_engine::mcp::tools::reflect_on_past(
                        engine.storage(),
                        engine.embeddings(),
                        engine.search(),
                        &conv.query_text,
                        LIMIT,
                        0.1,
                        project,
                    )
                })
                .await;
                if let Err(error) = result {
                    println!(
                        "  {source_label} {} [{scope_label}]: ERROR {error} — treated as unknown, not a miss",
                        conv.conversation_id
                    );
                    continue;
                }
                let final_hit = samples
                    .last()
                    .is_some_and(|sample| sample.target_rank.is_some());
                let entry = hit_counts.entry(source_label).or_insert((0, 0));
                entry.1 += 1;
                if source_label == "claude" && scope_label == "scoped" {
                    let templated = is_templated(&conv.content_norm);
                    let entry = claude_hit_by_template.entry(templated).or_insert((0, 0));
                    entry.1 += 1;
                    if final_hit {
                        entry.0 += 1;
                    }
                }
                let mut stage_label = String::new();
                if final_hit {
                    entry.0 += 1;
                } else {
                    let stage = losing_stage(&samples, LIMIT);
                    // Split an hnsw_fetch loss into ANN-recall-miss (brute
                    // force says it WAS within the fetched window's rank
                    // budget — HNSW's approximation missed it) versus
                    // semantic-miss (brute force agrees nothing that close
                    // existed within budget) using the ground truth loaded
                    // once at startup. Chunk-level on purpose (see
                    // `brute_force_rank`'s doc) — this comparison is against
                    // the HNSW fetch window, which is also chunk-level.
                    let classified = if stage == "hnsw_fetch" {
                        if let (Some(hnsw_sample), Some(qv)) = (
                            samples.iter().find(|s| s.stage == "hnsw_fetch"),
                            conv_query_vec.as_deref(),
                        ) {
                            let bf = brute_force_rank(
                                &all_rows,
                                qv,
                                &conv.conversation_id,
                                &HashSet::new(),
                            );
                            let window = hnsw_sample.total_candidates;
                            match bf.rank {
                                Some(r) if r < window => format!("{stage} (ann_recall_miss)"),
                                Some(_) => format!("{stage} (semantic_miss, outside window)"),
                                None => format!("{stage} (semantic_miss, no embedding match)"),
                            }
                        } else {
                            stage.clone()
                        }
                    } else {
                        stage.clone()
                    };
                    stage_label = classified.clone();
                    *miss_stage_counts
                        .entry((source_label, classified.clone()))
                        .or_insert(0) += 1;

                    // Spec 2(a): classify every semantic_miss (not the
                    // ann_recall_miss ones — those are an HNSW approximation
                    // problem, not a query/corpus content problem). Only
                    // meaningful once, so restrict to the "scoped" arm.
                    if scope_label == "scoped"
                        && classified.starts_with("hnsw_fetch (semantic_miss")
                    {
                        if let Some(scored) = &scored {
                            let target_ts = conv_meta
                                .get(&conv.conversation_id)
                                .map(|(ts, _)| ts.as_str())
                                .unwrap_or("");
                            let target_burst_ts = burst_key_ts(&conv.conversation_id, target_ts);
                            let target = MissTarget {
                                burst_key_ts: target_burst_ts,
                                harness_polluted: conv.harness_polluted,
                            };
                            let to_rows = |exclude: &HashSet<String>| -> Vec<SiblingRow> {
                                let filtered: Vec<&(f32, String)> = scored
                                    .iter()
                                    .filter(|(_, c)| !exclude.contains(c))
                                    .collect();
                                let rank = filtered
                                    .iter()
                                    .position(|(_, c)| c == &conv.conversation_id)
                                    .unwrap_or(filtered.len());
                                filtered[..rank]
                                    .iter()
                                    .map(|(score, sibling_id)| {
                                        let (sibling_ts, sibling_content) =
                                            conv_meta.get(sibling_id).cloned().unwrap_or_default();
                                        SiblingRow {
                                            cosine: *score,
                                            burst_key_ts: burst_key_ts(sibling_id, &sibling_ts),
                                            identical_text: sibling_content == conv.content_norm,
                                            contaminated: decontam_exclude.contains(sibling_id),
                                        }
                                    })
                                    .collect()
                            };
                            let rows_above_contam = to_rows(&HashSet::new());
                            let class_contam = classify_semantic_miss(&target, &rows_above_contam);
                            *miss_class_contam
                                .entry((source_label, class_contam))
                                .or_insert(0) += 1;

                            let rows_above_decontam = to_rows(&decontam_exclude);
                            let class_decontam =
                                classify_semantic_miss(&target, &rows_above_decontam);
                            *miss_class_decontam
                                .entry((source_label, class_decontam))
                                .or_insert(0) += 1;

                            println!(
                                "  {source_label:<6} {} miss_class contaminated={class_contam} decontaminated={class_decontam}",
                                conv.conversation_id
                            );
                        }
                    }
                }
                println!(
                    "  {source_label:<6} {} [{scope_label:<8} scope={resolved_scope_label:<12} q={:<16}] hit@6={} losing_stage={}",
                    conv.conversation_id,
                    conv.query_source,
                    final_hit,
                    if final_hit {
                        "-".to_string()
                    } else {
                        stage_label
                    },
                );
            }
        }
    }

    println!("\n--- hit@6 summary ---");
    for source in ["codex", "claude"] {
        if let Some((hits, total)) = hit_counts.get(source) {
            println!(
                "  {source}: {hits}/{total} = {:.1}%",
                100.0 * (*hits as f64) / (*total as f64).max(1.0)
            );
        }
    }
    println!(
        "\n--- decontaminated hit@6 summary (brute-force ground truth, excludes global \
         contamination set) ---"
    );
    for source in ["codex", "claude"] {
        if let Some((hits, total)) = decontam_hit_counts.get(source) {
            println!(
                "  {source}: {hits}/{total} = {:.1}%",
                100.0 * (*hits as f64) / (*total as f64).max(1.0)
            );
        }
    }
    println!(
        "\n--- decontaminated hit@6 summary (REAL pipeline, overfetch={PIPELINE_DECONTAM_OVERFETCH}, \
         contaminated conversations stripped from the final page before counting) ---"
    );
    for source in ["codex", "claude"] {
        if let Some((hits, total)) = pipeline_decontam_hit_counts.get(source) {
            println!(
                "  {source}: {hits}/{total} = {:.1}%",
                100.0 * (*hits as f64) / (*total as f64).max(1.0)
            );
        }
    }
    println!("\n--- miss stage histogram ---");
    let mut miss_rows: Vec<_> = miss_stage_counts.into_iter().collect();
    miss_rows.sort_by_key(|a| std::cmp::Reverse(a.1));
    for ((source, stage), count) in miss_rows {
        println!("  {source:<6} {stage:<40} {count}");
    }

    println!(
        "\n--- claude hit@6, templated vs non-templated first chunk (>=20 identical \
         first-120-char siblings = templated) ---"
    );
    for templated in [true, false] {
        if let Some((hits, total)) = claude_hit_by_template.get(&templated) {
            println!(
                "  templated={templated:<5} {hits}/{total} = {:.1}%",
                100.0 * (*hits as f64) / (*total as f64).max(1.0)
            );
        }
    }

    println!("\n--- semantic_miss class histogram (spec 2a) ---");
    println!("  [contaminated run — full corpus, no exclusion]");
    let mut contam_rows: Vec<_> = miss_class_contam.into_iter().collect();
    contam_rows.sort_by_key(|a| std::cmp::Reverse(a.1));
    for ((source, class), count) in &contam_rows {
        println!("    {source:<6} {class:<18} {count}");
    }
    println!("  [decontaminated run — global cutoff + per-query content match excluded]");
    let mut decontam_rows: Vec<_> = miss_class_decontam.into_iter().collect();
    decontam_rows.sort_by_key(|a| std::cmp::Reverse(a.1));
    for ((source, class), count) in &decontam_rows {
        println!("    {source:<6} {class:<18} {count}");
    }
    let dominant = |rows: &[((&str, MissClass), usize)], class: MissClass| -> usize {
        rows.iter()
            .filter(|((_, c), _)| *c == class)
            .map(|(_, n)| *n)
            .sum()
    };
    let total_decontam_misses: usize = decontam_rows.iter().map(|(_, n)| *n).sum();
    println!(
        "  spec 2(d) gate — decontaminated: burst_sibling={} template_sibling={} \
         harness_polluted={} contaminated={} genuine={} (total classified misses={})",
        dominant(&decontam_rows, MissClass::BurstSibling),
        dominant(&decontam_rows, MissClass::TemplateSibling),
        dominant(&decontam_rows, MissClass::HarnessPolluted),
        dominant(&decontam_rows, MissClass::Contaminated),
        dominant(&decontam_rows, MissClass::Genuine),
        total_decontam_misses,
    );

    Ok(())
}
