//! Dream backfill — Stage 5 verify (`.plans/dream-backfill-design.md` §3
//! "Stage 5 — verify", with the D6/D9/D11 round-2 deltas from §8 folded in,
//! since that section overrides §3-6 wherever they conflict).
//!
//! Zero LLM calls. Every candidate [`super::adjudicate`] decided REPLACED_BY
//! or EXTENDED_BY (D9's leg conjunction) lands here before anything is
//! promoted into a usable `dream_relations` row — an LLM verdict is a
//! HYPOTHESIS about a candidate that is itself already machine-generated
//! evidence; this stage is what turns "the model agreed" into "and here is
//! why that agreement can be trusted":
//!
//! - **Quotes** ([`quote_verified`]): length-scaled token-Jaccard against the
//!   episode record text the prompt actually contained (or its session
//!   chunks) — D6/D11. A quote the model invented rather than copied fails
//!   here.
//! - **OIDs** ([`verify_and_apply`]'s oid checks): every commit hash the
//!   model cited must independently resolve via `git cat-file` in the
//!   project's repo AND appear somewhere in this project's ledger evidence
//!   (`witness_ledger.at_oid` / `witness_verdicts.receipt_oid` /
//!   `observed_head_oid`) — D6. Adapting D6's literal "ledger stamp set"
//!   wording: `witness_ledger.stamp` is a BLAKE3 CONTENT hash, not an OID
//!   (see [`super::pairs`]'s module doc for the same body_hash/stamp
//!   confusion at the pair-generation stage) — the OID-shaped columns
//!   (`at_oid`/`receipt_oid`/`observed_head_oid`) are what a citation must
//!   appear in instead. The candidate's own `load_bearing_oid` is
//!   re-resolved the same way when its `oid_provenance` claims it was
//!   git-derived (a fresh resolution can catch drift since generation
//!   time); its `aux_oid` is never a discard reason (D6: "auxiliary OIDs
//!   downgrade not discard").
//! - **Direction** (`ts(A) < ts(B)`) is re-checked in SQL, never trusted
//!   from what was loaded in Rust — the design's explicit "the LLM is never
//!   trusted with ordering".
//! - **Tier ceiling** ([`super::adjudicate::TIER_CEILING`]): a pass never
//!   promotes past `witnessed` (D5).
//!
//! Any failure discards the candidate: `dream_relations.status` moves to
//! `'archived'` (never re-adjudicated on the next run — the LLM's own
//! agreement already cost a budget slot; there is no reason to spend
//! another on the same disproven pair) and a `backfill_discards` row
//! records the reason and the raw response, per the design's own framing of
//! that table ("Stage 5 verify-failure audit log").

use std::collections::{HashMap, HashSet};
use std::process::Command;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use crate::temporal::parse_timestamp;

use super::adjudicate::{
    episode_record_text, EpisodeFacts, QueuedRelation, RawVerdict, TIER_CEILING,
};
use super::claim_resolution::{fill_pair_quotes, QuoteSlot};
use super::pairs::Relation;

pub(super) enum VerifyOutcome {
    Passed,
    Failed(&'static str),
}

// ---------------------------------------------------------------------
// backfill_discards / dream_relations disposition writers
// ---------------------------------------------------------------------

pub(super) fn pair_key(project: &str, ep_a: &str, ep_b: &str) -> String {
    format!("{project}:{ep_a}:{ep_b}")
}

pub(super) fn log_discard(
    conn: &Connection,
    pair_key: &str,
    reason: &str,
    raw_json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO backfill_discards (pair_key, reason, raw_json) VALUES (?1, ?2, ?3)",
        params![pair_key, reason, raw_json],
    )?;
    Ok(())
}

/// UNRELATED (a valid negative D9 outcome, never a discard) and verify
/// failures both land here: never re-adjudicated again, but the row itself
/// stays as an audit trail rather than being deleted.
pub(super) fn archive_relation(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE dream_relations SET status = 'archived' WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

/// B: `UNIQUE(project, ep_a, ep_b, relation)` protects `dream_relations`
/// against exactly the collision a bare `UPDATE ... SET relation = ?1 ...
/// WHERE id = ?5` would otherwise throw on — a sibling row already holding
/// `(project, ep_a, ep_b, <decided relation>)` under a DIFFERENT id. This
/// can happen for twin hypothesis rows queued for the same story (P3's
/// cross-generator dedup narrows this to zero within a single
/// `gate_project` pass, but does not close it for a pre-existing row from
/// an earlier run, or any future path that inserts a duplicate outside
/// `gate_project` entirely) or simply because two SEPARATE candidates
/// happened to be adjudicated to the same final relation before P3 ever ran.
/// Rather than let that `UPDATE` raise a constraint error and abort
/// [`super::adjudicate::run_adjudication_with`] mid-loop, check for the
/// conflicting sibling FIRST: if one exists, archive this candidate with a
/// `duplicate_story` discard instead of promoting it — the sibling already
/// carries the story, and there is no signal lost by not re-promoting a
/// second row for the same fact.
fn promote_relation(
    conn: &Connection,
    candidate: &QueuedRelation,
    relation: Relation,
    quote_a: &str,
    quote_b: &str,
    raw_json: &str,
) -> Result<VerifyOutcome> {
    let conflict: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, status FROM dream_relations
             WHERE project = ?1 AND ep_a = ?2 AND ep_b = ?3 AND relation = ?4 AND id != ?5",
            params![
                candidate.project,
                candidate.ep_a,
                candidate.ep_b,
                relation.as_str(),
                candidate.id
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((_, status)) = conflict {
        // Round-4 F-R6: a sibling that was UNRELATED-archived is not the
        // same thing as a live duplicate — the audit reason must not claim
        // "story already told" when the sibling was in fact rejected.
        let reason = if status == "archived" {
            "previously_unrelated"
        } else {
            "duplicate_story"
        };
        return discard(conn, candidate, reason, raw_json);
    }

    conn.execute(
        "UPDATE dream_relations SET relation = ?1, tier = ?2, quote_a = ?3, quote_b = ?4 WHERE id = ?5",
        params![relation.as_str(), TIER_CEILING, quote_a, quote_b, candidate.id],
    )?;
    Ok(VerifyOutcome::Passed)
}

fn discard(
    conn: &Connection,
    candidate: &QueuedRelation,
    reason: &'static str,
    raw_json: &str,
) -> Result<VerifyOutcome> {
    log_discard(
        conn,
        &pair_key(&candidate.project, &candidate.ep_a, &candidate.ep_b),
        reason,
        raw_json,
    )?;
    archive_relation(conn, candidate.id)?;
    Ok(VerifyOutcome::Failed(reason))
}

// ---------------------------------------------------------------------
// Quote verification (D6/D11)
// ---------------------------------------------------------------------

fn tokenize(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split_whitespace()
        .map(|t| t.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// P7 (F7/T4): a short, closed list of function words excluded from a
/// quote's CONTENT tokens before matching. A quote's evidentiary weight
/// lives in its content words ("parse_frame", "heartbeat") — stopwords carry
/// none, and (per T4's adversarial case) a stopword-only "quote" must never
/// pass just because every one of its tokens is common.
fn is_stopword(t: &str) -> bool {
    matches!(
        t,
        "a" | "an"
            | "the"
            | "to"
            | "in"
            | "on"
            | "of"
            | "and"
            | "or"
            | "is"
            | "are"
            | "was"
            | "were"
            | "be"
            | "been"
            | "being"
            | "it"
            | "its"
            | "this"
            | "that"
            | "these"
            | "those"
            | "we"
            | "i"
            | "you"
            | "they"
            | "he"
            | "she"
            | "for"
            | "with"
            | "as"
            | "at"
            | "by"
            | "from"
            | "but"
            | "not"
            | "no"
            | "do"
            | "does"
            | "did"
            | "so"
            | "if"
            | "then"
            | "than"
            | "too"
            | "very"
            | "just"
            | "about"
            | "into"
            | "over"
            | "under"
            | "also"
            | "via"
    )
}

/// Minimum surviving content tokens for a quote to be checkable at all —
/// below this, a "match" would prove nothing (a single shared word is
/// coincidence, not a quotation).
const QUOTE_MIN_CONTENT_HITS: usize = 2;
/// Fraction of a quote's content tokens that must appear, in order, inside
/// some source window for the quote to count as verified.
const QUOTE_COVERAGE_MIN: f64 = 0.80;
/// A candidate window may run up to this multiple of the quote's own token
/// length — the slack that forgives INSERTION noise (extra source tokens
/// interleaved between the quote's real words) that a fixed-length window
/// cannot (T4/F7: the shipped `(n-1)/(n+1)` curve assumed substitution
/// noise only and false-discarded a real quote with three extra
/// interleaved source tokens).
const QUOTE_WINDOW_MAX_FACTOR: usize = 2;
/// Minimum fraction of the quote's content-token ORDER that must be
/// preserved (as a longest-common-subsequence against the matched window) —
/// the gate that keeps a scrambled bag of the right words from passing as a
/// quotation (T4's "token-order permutation" adversarial case).
const QUOTE_LCS_MIN_FRACTION: f64 = 0.5;

/// Length of the longest common subsequence between two token sequences —
/// the standard O(n*m) DP. Used only for short quote-vs-window sequences
/// (bounded by `QUOTE_WINDOW_MAX_FACTOR`), never the whole source document.
fn lcs_len(a: &[&str], b: &[&str]) -> usize {
    let mut dp = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            dp[i][j] = if a[i - 1] == b[j - 1] {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
        }
    }
    dp[a.len()][b.len()]
}

/// P7 (F7): content-token coverage over an expandable window, replacing the
/// shipped fixed-length windowed-set-Jaccard matcher wholesale (T4's audit
/// of that curve: correct for exactly one noise model — a contiguous,
/// equal-length window with one SUBSTITUTED token — and false-discarding on
/// every other realistic deviation, most importantly insertion noise).
///
/// A quote passes against a source when some window of length `|Q|..2|Q|`
/// contains at least [`QUOTE_COVERAGE_MIN`] of the quote's non-stopword
/// (content) tokens, at least [`QUOTE_MIN_CONTENT_HITS`] of them, in an
/// order that preserves at least [`QUOTE_LCS_MIN_FRACTION`] of the content
/// sequence (the LCS gate — order matters, a scrambled bag of the right
/// words is not a quotation). No stemming: exact token equality only
/// (`frames != frame`, `parsing != parser`) — a deliberate precision choice
/// (T4), not an oversight.
fn quote_matches(quote: &str, source: &str) -> bool {
    let q = tokenize(quote);
    if q.is_empty() {
        return false;
    }
    let s = tokenize(source);
    if s.is_empty() {
        return false;
    }
    let content: Vec<&str> = q
        .iter()
        .map(String::as_str)
        .filter(|t| !is_stopword(t))
        .collect();
    if content.len() < QUOTE_MIN_CONTENT_HITS {
        return false; // stopword-only or single-content-token quotes prove nothing
    }
    let content_set: HashSet<&str> = content.iter().copied().collect();
    let min_w = q.len();
    let max_w = (q.len() * QUOTE_WINDOW_MAX_FACTOR).min(s.len()).max(min_w);

    let mut best_hits = 0usize;
    let mut best_window: Vec<&str> = Vec::new();
    for start in 0..s.len() {
        let mut window: HashSet<&str> = HashSet::new();
        let mut seq: Vec<&str> = Vec::new();
        for end in start..s.len() {
            window.insert(s[end].as_str());
            seq.push(s[end].as_str());
            let len = end - start + 1;
            if len < min_w.min(s.len()) {
                continue;
            }
            if len > max_w {
                break;
            }
            let hits = content_set.intersection(&window).count();
            if hits > best_hits {
                best_hits = hits;
                best_window = seq.clone();
            }
        }
    }

    if best_hits < QUOTE_MIN_CONTENT_HITS.min(content.len()) {
        return false;
    }
    if (best_hits as f64 / content.len() as f64) < QUOTE_COVERAGE_MIN {
        return false;
    }
    (lcs_len(&content, &best_window) as f64 / content.len() as f64) >= QUOTE_LCS_MIN_FRACTION
}

/// A quote passes if it matches EITHER the episode record text or one of
/// the session chunks (D6: "against the source episode record (or its
/// session chunks, fetched by conversation_id)"). An empty quote never
/// passes -- [`super::adjudicate::decide_relation`] already requires both
/// `quote_a_attests_a`/`quote_b_attests_b` true to reach here, so a genuine
/// attestation with an empty quote is itself a fabrication signal.
pub(super) fn quote_verified(quote: &str, record_text: &str, chunk_texts: &[String]) -> bool {
    if tokenize(quote).is_empty() {
        return false;
    }
    if quote_matches(quote, record_text) {
        return true;
    }
    chunk_texts.iter().any(|c| quote_matches(quote, c))
}

/// P7 (F8): audit-trail classifier only -- NEVER affects keep/discard, only
/// which of `fabricated_quote_*` / `misattributed_quote_*` a failed quote is
/// logged under. If a quote that failed against its CLAIMED episode
/// verifiably lives in some OTHER episode's record, the model grabbed the
/// right words from the wrong episode -- a materially different failure
/// mode from pure invention, worth distinguishing in `backfill_discards`.
fn quote_found_in_other_episode(
    conn: &Connection,
    quote: &str,
    candidate: &QueuedRelation,
) -> Result<bool> {
    // Family scope, same predicate and rationale as `oid_in_ledger`:
    // episode rows keep raw keys while the candidate carries the family
    // name. Audit-trail-only, so the stakes are classification fidelity
    // (misattributed vs fabricated), never keep/discard.
    let mut stmt = conn.prepare(
        "SELECT episode_id, request, completed, next_steps, blockers, files_json
         FROM episode_index
         WHERE lower(project) = lower(?1) OR lower(project) LIKE lower(?1) || '-%'",
    )?;
    let rows = stmt
        .query_map(params![candidate.project], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, String>(5)?,
                ),
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, (req, comp, next, blk, files_json)) in rows {
        if id == candidate.ep_a || id == candidate.ep_b {
            continue;
        }
        let mut text = format!(
            "{} {} {} {}",
            req,
            comp,
            next.as_deref().unwrap_or(""),
            blk.as_deref().unwrap_or("")
        );
        // Files-line parity with `episode_record_text` (round-4 F-R7): a
        // quote spanning the "Files touched:" line must classify as
        // misattributed, not fabricated, when it lives in a third episode.
        if let Ok(files) = serde_json::from_str::<Vec<String>>(&files_json) {
            if !files.is_empty() {
                text.push_str(" Files touched: ");
                text.push_str(&files.join(", "));
            }
        }
        if quote_matches(quote, &text) {
            return Ok(true);
        }
    }
    Ok(false)
}

// ---------------------------------------------------------------------
// OID resolution (D6)
// ---------------------------------------------------------------------

/// `git -C <repo_root>` with ambient `GIT_*` env stripped -- same rationale
/// and pattern as `dream::backfill::pairs::git_at` /
/// `storage::dream_backfill::git_at`, duplicated locally per this
/// codebase's convention of keeping each module's small git helpers
/// dependency-free of its siblings.
fn git_at(repo_root: &str) -> Command {
    let mut cmd = Command::new("git");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&k);
        }
    }
    cmd.arg("-C").arg(repo_root);
    cmd
}

/// Does `oid` resolve to a real commit in `repo_root`? `git cat-file -e
/// <oid>^{commit}` -- the `^{commit}` peel means a blob/tree hash that
/// merely happens to exist never passes as a commit citation.
fn git_oid_exists(repo_root: &str, oid: &str) -> bool {
    let spec = format!("{oid}^{{commit}}");
    git_at(repo_root)
        .arg("cat-file")
        .arg("-e")
        .arg(&spec)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// D6's "ledger stamp set", adapted per the module doc: the OID-shaped
/// columns, not `witness_ledger.stamp` (a content hash, not an OID).
///
/// P9 (F9): project-scoped. Without the scope, a commit hash sitting in a
/// DIFFERENT project's ledger evidence would validate a citation here —
/// two unrelated projects sharing a monorepo (or just coincidentally
/// overlapping short-hash-adjacent OIDs) would let a fabricated-for-this-
/// project citation "trust" itself via someone else's real commit.
///
/// FAMILY scope (2026-08-26 ruling): `dream_relations.project` now carries
/// the family name, while ledger rows keep their raw keys — the scope is
/// therefore "the project or any hyphen-extension of it"
/// (`super::family::group_keys`'s own invariant: every member of a
/// computed family extends the family name at a hyphen boundary, because
/// grouping is prefix-transitive through the shortest key). Unrelated
/// projects still never match — that is exactly what P9 protects.
fn oid_in_ledger(conn: &Connection, project: &str, oid: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM (
            SELECT 1 FROM witness_ledger
             WHERE at_oid = ?1
               AND (lower(project) = lower(?2) OR lower(project) LIKE lower(?2) || '-%')
            UNION ALL
            SELECT 1 FROM witness_verdicts v
              JOIN witness_ledger w ON w.id = v.witness_id
             WHERE (v.receipt_oid = ?1 OR v.observed_head_oid = ?1)
               AND (lower(w.project) = lower(?2) OR lower(w.project) LIKE lower(?2) || '-%')
         )",
        params![oid, project],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

/// Per-run cache keyed `(repo_root, oid)` -- D6: "cache resolvability per
/// distinct OID". Git resolution is the expensive half of an OID check, so
/// the cache is keyed on it alone; `oid_in_ledger` is a cheap indexed query
/// and is not cached.
pub(super) struct OidCache(HashMap<(String, String), bool>);

impl OidCache {
    pub(super) fn new() -> Self {
        Self(HashMap::new())
    }

    fn git_resolves(&mut self, repo_root: &str, oid: &str) -> bool {
        *self
            .0
            .entry((repo_root.to_string(), oid.to_string()))
            .or_insert_with(|| git_oid_exists(repo_root, oid))
    }
}

fn resolve_repo_root(a: &EpisodeFacts, b: &EpisodeFacts) -> Option<String> {
    a.files
        .iter()
        .chain(b.files.iter())
        .find_map(|f| crate::extraction::repo_root::repo_root_for_file(f))
}

/// Is `oid` trustworthy: independently git-resolvable AND already part of
/// this project's ledger evidence? Both must hold -- either alone is not
/// enough to trust a citation the model produced from free text.
fn oid_trusted(
    conn: &Connection,
    project: &str,
    repo_root: Option<&str>,
    oid_cache: &mut OidCache,
    oid: &str,
) -> Result<bool> {
    if !oid_in_ledger(conn, project, oid)? {
        return Ok(false);
    }
    Ok(repo_root
        .map(|r| oid_cache.git_resolves(r, oid))
        .unwrap_or(false))
}

// ---------------------------------------------------------------------
// Direction (never trust the LLM with ordering)
// ---------------------------------------------------------------------

/// P8 (F6): re-parses both timestamps via `crate::temporal::parse_timestamp`
/// — the same parser every other pipeline stage uses — rather than
/// delegating to SQLite's `julianday(?)`, whose acceptance of the `Z` suffix
/// is a dialect/build detail (the bundled `libsqlite3-sys` engine and a
/// system `sqlite3` can disagree). A NULL `julianday` result on an
/// unexpected format would make EVERY candidate fail `ts_ordering` silently
/// — a catastrophic false-discard mode this rewrite removes entirely.
/// Either timestamp failing to parse resolves to `false` (order cannot be
/// asserted, so verify's caller discards -- the same discard-safe direction
/// an unparseable ts already takes everywhere else in this pipeline).
fn ts_ordered(_conn: &Connection, ts_a: &str, ts_b: &str) -> Result<bool> {
    let (Some(a), Some(b)) = (parse_timestamp(ts_a), parse_timestamp(ts_b)) else {
        return Ok(false);
    };
    Ok(a < b)
}

// ---------------------------------------------------------------------
// The whole Stage 5 check, in order, short-circuiting on the first failure
// ---------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub(super) fn verify_and_apply(
    conn: &Connection,
    candidate: &QueuedRelation,
    a: &EpisodeFacts,
    a_chunks: &[String],
    b: &EpisodeFacts,
    b_chunks: &[String],
    relation: Relation,
    verdict: &RawVerdict,
    oid_cache: &mut OidCache,
    raw_json: &str,
) -> Result<VerifyOutcome> {
    // Direction: re-checked in SQL, never trusted from the Rust-side ts
    // strings the pipeline already loaded.
    if !ts_ordered(conn, &a.ts, &b.ts)? {
        return discard(conn, candidate, "ts_ordering", raw_json);
    }

    // Quotes: against exactly the text the prompt contained. P7 (F8):
    // empty is checked first and separately (its own reason, not folded
    // into "fabricated"); a failed non-empty quote is further classified as
    // `misattributed_*` (found verbatim in some OTHER episode's record) vs.
    // `fabricated_*` (found nowhere at all) -- audit-trail only, never
    // affects the discard itself.
    if verdict.quote_a.trim().is_empty() {
        return discard(conn, candidate, "empty_quote_a", raw_json);
    }
    if !quote_verified(&verdict.quote_a, &episode_record_text(a), a_chunks) {
        let reason: &'static str =
            if quote_found_in_other_episode(conn, &verdict.quote_a, candidate)? {
                "misattributed_quote_a"
            } else {
                "fabricated_quote_a"
            };
        return discard(conn, candidate, reason, raw_json);
    }
    if verdict.quote_b.trim().is_empty() {
        return discard(conn, candidate, "empty_quote_b", raw_json);
    }
    if !quote_verified(&verdict.quote_b, &episode_record_text(b), b_chunks) {
        let reason: &'static str =
            if quote_found_in_other_episode(conn, &verdict.quote_b, candidate)? {
                "misattributed_quote_b"
            } else {
                "fabricated_quote_b"
            };
        return discard(conn, candidate, reason, raw_json);
    }

    // OIDs the model cited as backing evidence.
    let repo_root = resolve_repo_root(a, b);
    for oid in &verdict.oids {
        if !oid_trusted(
            conn,
            &candidate.project,
            repo_root.as_deref(),
            oid_cache,
            oid,
        )? {
            return discard(conn, candidate, "fake_oid", raw_json);
        }
    }

    // The candidate's own load-bearing OID, re-resolved (D6): only a claim
    // of git-derived provenance is held to this bar -- a fallback-provenance
    // row never had a resolvable OID to begin with (see `pairs`'s module
    // doc), and `aux_oid` is never a discard reason at all.
    if candidate.oid_provenance == "git_derived" {
        if let Some(lb) = &candidate.load_bearing_oid {
            let resolves = repo_root
                .as_deref()
                .map(|r| oid_cache.git_resolves(r, lb))
                .unwrap_or(false);
            if !resolves {
                return discard(conn, candidate, "load_bearing_oid_unresolvable", raw_json);
            }
        }
    }

    promote_relation(
        conn,
        candidate,
        relation,
        &verdict.quote_a,
        &verdict.quote_b,
        raw_json,
    )
}

/// Deterministic promotion for anchor-evidenced generators (ledger and
/// relapse) — the round-5 empirical amendment. The first two live runs
/// proved textual quote-attestation via the unprimed LLM judge is
/// IMPOSSIBLE for these pairs: their symbols come from AST anchors of
/// edited files and appear in neither the episode prose nor the session
/// chunks (0 of 16 queued pairs had a single symbol-bearing chunk), so an
/// honest unprimed judge must answer UNRELATED every time. Their relation
/// evidence was never textual to begin with: a governing ledger verdict, a
/// receipt OID, and a symbol re-touch with a different body hash — all
/// machine-checked. This path applies exactly the checks that evidence
/// supports (SQL timestamp ordering + load-bearing OID re-resolution) —
/// the LLM stays reserved for era pairs, whose evidence genuinely lives in
/// prose.
///
/// F2 fix (Codex review pass 1, finding #2): this used to promote with
/// `quote_a = quote_b = ""` — the exact "thin dream" bug B was built to
/// close but was never wired to. It is now wired here:
/// [`fill_pair_quotes`] pulls each side's own `request`/`completed` text,
/// verbatim-extracted and byte-verified against that episode's own
/// narrative fields (see [`super::claim_resolution`]'s module doc for what
/// that haystack is and is not). A [`QuoteSlot::Pointer`] (nothing verbatim
/// locatable — including an empty `request`/`completed` field, which
/// [`super::claim_resolution::extract_quote`] deliberately never turns into
/// a fabricated quote) is NOT accepted as satisfying this path's
/// non-empty-quote contract: the candidate is discarded with the same
/// `empty_quote_a`/`empty_quote_b` reasons [`verify_and_apply`] uses, never
/// silently promoted with nothing to show.
pub(super) fn verify_and_apply_deterministic(
    conn: &Connection,
    candidate: &QueuedRelation,
    a: &EpisodeFacts,
    b: &EpisodeFacts,
    relation: Relation,
    oid_cache: &mut OidCache,
) -> Result<VerifyOutcome> {
    const RAW: &str = r#"{"deterministic":true}"#;
    if !ts_ordered(conn, &a.ts, &b.ts)? {
        return discard(conn, candidate, "ts_ordering", RAW);
    }
    if candidate.oid_provenance == "git_derived" {
        if let Some(lb) = &candidate.load_bearing_oid {
            let repo_root = resolve_repo_root(a, b);
            let resolves = repo_root
                .as_deref()
                .map(|r| oid_cache.git_resolves(r, lb))
                .unwrap_or(false);
            if !resolves {
                return discard(conn, candidate, "load_bearing_oid_unresolvable", RAW);
            }
        }
    }

    let (quote_a, quote_b) = match fill_pair_quotes(conn, &candidate.ep_a, &candidate.ep_b) {
        Ok(q) => q,
        Err(_) => return discard(conn, candidate, "quote_extraction_failed", RAW),
    };
    let QuoteSlot::Verbatim { text: text_a, .. } = &quote_a else {
        return discard(conn, candidate, "empty_quote_a", RAW);
    };
    let QuoteSlot::Verbatim { text: text_b, .. } = &quote_b else {
        return discard(conn, candidate, "empty_quote_b", RAW);
    };

    promote_relation(conn, candidate, relation, text_a, text_b, RAW)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> crate::storage::Storage {
        crate::storage::Storage::open_memory().unwrap()
    }

    fn mk_facts(ts: &str, text: &str) -> EpisodeFacts {
        EpisodeFacts {
            episode_id: "e".to_string(),
            session_id: "s".to_string(),
            ts: ts.to_string(),
            request: text.to_string(),
            completed: String::new(),
            next_steps: None,
            blockers: None,
            files: vec![],
        }
    }

    fn mk_candidate(oid_provenance: &str, load_bearing_oid: Option<&str>) -> QueuedRelation {
        QueuedRelation {
            id: 1,
            project: "p".to_string(),
            ep_a: "a".to_string(),
            ep_b: "b".to_string(),
            topic_key: "symbol:foo".to_string(),
            generator: "ledger".to_string(),
            relation: "replaced_by".to_string(),
            load_bearing_oid: load_bearing_oid.map(|s| s.to_string()),
            oid_provenance: oid_provenance.to_string(),
        }
    }

    fn mk_verdict(quote_a: &str, quote_b: &str, oids: Vec<String>) -> RawVerdict {
        RawVerdict {
            quote_a_attests_a: true,
            quote_b_attests_b: true,
            incompatible: true,
            same_approach: false,
            extended: false,
            quote_a: quote_a.to_string(),
            quote_b: quote_b.to_string(),
            oids,
        }
    }

    fn seed_dream_relation(storage: &crate::storage::Storage) -> i64 {
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO dream_relations
                        (project, ep_a, ep_b, relation, generator, topic_key, tier, status)
                     VALUES ('p', 'a', 'b', 'replaced_by', 'ledger', 'symbol:foo', 'unverified', 'queued')",
                    [],
                )?;
                Ok(conn.last_insert_rowid())
            })
            .unwrap()
    }

    // -----------------------------------------------------------------
    // Quote Jaccard threshold + windowing
    // -----------------------------------------------------------------

    #[test]
    fn quote_verified_accepts_exact_substring() {
        let record = "we decided to always retry on timeout errors before giving up";
        assert!(quote_verified(
            "always retry on timeout errors",
            record,
            &[]
        ));
    }

    #[test]
    fn quote_verified_rejects_a_fabricated_quote() {
        let record = "we decided to always retry on timeout errors before giving up";
        assert!(!quote_verified(
            "we decided to never retry and just crash immediately",
            record,
            &[]
        ));
    }

    #[test]
    fn quote_verified_falls_back_to_session_chunks() {
        let record = "short summary only";
        let chunks = vec!["the real detail: always retry on timeout errors first".to_string()];
        assert!(quote_verified(
            "always retry on timeout errors",
            record,
            &chunks
        ));
    }

    #[test]
    fn quote_verified_rejects_an_empty_quote() {
        assert!(!quote_verified("", "anything at all here", &[]));
    }

    // -----------------------------------------------------------------
    // Fabricated quote discarded end-to-end through verify_and_apply
    // -----------------------------------------------------------------

    #[test]
    fn verify_and_apply_discards_a_fabricated_quote() {
        let storage = open();
        let id = seed_dream_relation(&storage);
        let candidate = QueuedRelation {
            id,
            ..mk_candidate("created_at_fallback", None)
        };
        let a = mk_facts("2020-01-01T00:00:00Z", "we always retry on timeout errors");
        let b = mk_facts("2020-02-01T00:00:00Z", "we now fail fast on timeout errors");
        let verdict = mk_verdict(
            "this quote was never actually said by anyone in episode A",
            "we now fail fast on timeout errors",
            vec![],
        );

        let outcome = storage
            .with_connection(|conn| {
                let mut cache = OidCache::new();
                verify_and_apply(
                    conn,
                    &candidate,
                    &a,
                    &[],
                    &b,
                    &[],
                    Relation::ReplacedBy,
                    &verdict,
                    &mut cache,
                    "{}",
                )
            })
            .unwrap();
        assert!(matches!(
            outcome,
            VerifyOutcome::Failed("fabricated_quote_a")
        ));

        let (status, discards): (String, i64) = storage
            .with_connection(|conn| {
                let status: String = conn.query_row(
                    "SELECT status FROM dream_relations WHERE id = ?1",
                    [id],
                    |r| r.get(0),
                )?;
                let discards: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM backfill_discards WHERE reason = 'fabricated_quote_a'",
                    [],
                    |r| r.get(0),
                )?;
                Ok((status, discards))
            })
            .unwrap();
        assert_eq!(status, "archived");
        assert_eq!(discards, 1);
    }

    // -----------------------------------------------------------------
    // Fake OID discarded
    // -----------------------------------------------------------------

    #[test]
    fn verify_and_apply_discards_a_fake_oid_not_in_the_ledger() {
        let storage = open();
        let id = seed_dream_relation(&storage);
        let candidate = QueuedRelation {
            id,
            ..mk_candidate("created_at_fallback", None)
        };
        let a = mk_facts("2020-01-01T00:00:00Z", "we always retry on timeout errors");
        let b = mk_facts("2020-02-01T00:00:00Z", "we now fail fast on timeout errors");
        let verdict = mk_verdict(
            "we always retry on timeout errors",
            "we now fail fast on timeout errors",
            vec!["deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string()],
        );

        let outcome = storage
            .with_connection(|conn| {
                let mut cache = OidCache::new();
                verify_and_apply(
                    conn,
                    &candidate,
                    &a,
                    &[],
                    &b,
                    &[],
                    Relation::ReplacedBy,
                    &verdict,
                    &mut cache,
                    "{}",
                )
            })
            .unwrap();
        assert!(matches!(outcome, VerifyOutcome::Failed("fake_oid")));
    }

    #[test]
    fn verify_and_apply_trusts_an_oid_actually_recorded_in_the_ledger() {
        use crate::storage::witness_ledger::{insert_witness, WitnessLedgerRow};

        let storage = open();
        let id = seed_dream_relation(&storage);
        storage
            .with_connection(|conn| {
                insert_witness(
                    conn,
                    &WitnessLedgerRow {
                        id: 0,
                        project: "p".to_string(),
                        file: "a.rs".to_string(),
                        symbol: Some("foo".to_string()),
                        span_start: None,
                        span_end: None,
                        stamp: "b3:aaa".to_string(),
                        tier: "committed".to_string(),
                        at_oid: Some("realoid1234".to_string()),
                        source_kind: "backfill".to_string(),
                        source_id: None,
                    },
                )
            })
            .unwrap();

        // No local file resolves to a real git repo in this test, so
        // `git_resolves` is unreachable -- `oid_in_ledger` alone is what is
        // under test here (the short-circuit in `oid_trusted`).
        let candidate = QueuedRelation {
            id,
            ..mk_candidate("created_at_fallback", None)
        };
        let a = mk_facts("2020-01-01T00:00:00Z", "we always retry on timeout errors");
        let b = mk_facts("2020-02-01T00:00:00Z", "we now fail fast on timeout errors");
        let verdict = mk_verdict(
            "we always retry on timeout errors",
            "we now fail fast on timeout errors",
            vec!["realoid1234".to_string()],
        );

        let outcome = storage
            .with_connection(|conn| {
                let mut cache = OidCache::new();
                verify_and_apply(
                    conn,
                    &candidate,
                    &a,
                    &[],
                    &b,
                    &[],
                    Relation::ReplacedBy,
                    &verdict,
                    &mut cache,
                    "{}",
                )
            })
            .unwrap();
        // The oid is in the ledger but cannot git-resolve (no real repo) --
        // `oid_trusted` requires BOTH, so this still fails, just for the
        // OTHER half of the AND. Proves the ledger check alone is not
        // sufficient to pass, matching the "both must hold" doc comment.
        assert!(matches!(outcome, VerifyOutcome::Failed("fake_oid")));
    }

    // -----------------------------------------------------------------
    // ts ordering re-check
    // -----------------------------------------------------------------

    #[test]
    fn verify_and_apply_discards_when_direction_is_reversed() {
        let storage = open();
        let id = seed_dream_relation(&storage);
        let candidate = QueuedRelation {
            id,
            ..mk_candidate("created_at_fallback", None)
        };
        // Reversed on purpose: a's ts is AFTER b's.
        let a = mk_facts("2099-01-01T00:00:00Z", "we always retry on timeout errors");
        let b = mk_facts("2020-01-01T00:00:00Z", "we now fail fast on timeout errors");
        let verdict = mk_verdict(
            "we always retry on timeout errors",
            "we now fail fast on timeout errors",
            vec![],
        );

        let outcome = storage
            .with_connection(|conn| {
                let mut cache = OidCache::new();
                verify_and_apply(
                    conn,
                    &candidate,
                    &a,
                    &[],
                    &b,
                    &[],
                    Relation::ReplacedBy,
                    &verdict,
                    &mut cache,
                    "{}",
                )
            })
            .unwrap();
        assert!(matches!(outcome, VerifyOutcome::Failed("ts_ordering")));
    }

    // -----------------------------------------------------------------
    // A clean pass promotes and caps at the tier ceiling
    // -----------------------------------------------------------------

    #[test]
    fn verify_and_apply_promotes_a_clean_pass_to_the_tier_ceiling() {
        let storage = open();
        let id = seed_dream_relation(&storage);
        let candidate = QueuedRelation {
            id,
            ..mk_candidate("created_at_fallback", None)
        };
        let a = mk_facts("2020-01-01T00:00:00Z", "we always retry on timeout errors");
        let b = mk_facts("2020-02-01T00:00:00Z", "we now fail fast on timeout errors");
        let verdict = mk_verdict(
            "we always retry on timeout errors",
            "we now fail fast on timeout errors",
            vec![],
        );

        let outcome = storage
            .with_connection(|conn| {
                let mut cache = OidCache::new();
                verify_and_apply(
                    conn,
                    &candidate,
                    &a,
                    &[],
                    &b,
                    &[],
                    Relation::ExtendedBy,
                    &verdict,
                    &mut cache,
                    "{}",
                )
            })
            .unwrap();
        assert!(matches!(outcome, VerifyOutcome::Passed));

        let (tier, status, relation, quote_a): (String, String, String, String) = storage
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT tier, status, relation, quote_a FROM dream_relations WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(tier, TIER_CEILING);
        assert_eq!(
            status, "queued",
            "promotion never changes status -- Stage 6 drains it"
        );
        assert_eq!(relation, "extended_by");
        assert_eq!(quote_a, "we always retry on timeout errors");
    }

    // -----------------------------------------------------------------
    // F2 (Codex review pass 1, finding #2): the deterministic (ledger/
    // relapse) promotion path must actually be wired to non-empty,
    // byte-verified verbatim quotes -- the exact "thin dream" bug this
    // closes -- and must never accept an empty field's Pointer fallback as
    // satisfying that contract.
    // -----------------------------------------------------------------

    fn insert_episode_for_quotes(
        storage: &crate::storage::Storage,
        id: &str,
        ts: &str,
        request: &str,
        completed: &str,
    ) {
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO reflections (id, content, tags, timestamp) VALUES (?1, ?2, '[]', ?3)",
                    params![
                        id,
                        format!(
                            r#"{{"schema":"v2","session_id":"{id}","project":"p","timestamp":"{ts}","request":{request},"completed":{completed},"outcome":"completed","todos":[],"files_modified":[],"anchors":[]}}"#,
                            request = serde_json::to_string(request).unwrap(),
                            completed = serde_json::to_string(completed).unwrap(),
                        ),
                        ts
                    ],
                )?;
                crate::storage::dream_backfill::materialize_episode_index(conn)
            })
            .unwrap();
    }

    fn seed_deterministic_dream_relation(
        storage: &crate::storage::Storage,
        ep_a: &str,
        ep_b: &str,
    ) -> i64 {
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO dream_relations
                        (project, ep_a, ep_b, relation, generator, topic_key, tier, status, oid_provenance)
                     VALUES ('p', ?1, ?2, 'replaced_by', 'ledger', 'symbol:foo', 'unverified', 'queued', 'created_at_fallback')",
                    params![ep_a, ep_b],
                )?;
                Ok(conn.last_insert_rowid())
            })
            .unwrap()
    }

    #[test]
    fn verify_and_apply_deterministic_wires_non_empty_verbatim_quotes() {
        let storage = open();
        insert_episode_for_quotes(
            &storage,
            "ep-det-a",
            "2020-01-01T00:00:00Z",
            "fix the retry loop",
            "did some prep",
        );
        insert_episode_for_quotes(
            &storage,
            "ep-det-b",
            "2020-02-01T00:00:00Z",
            "some later ask",
            "fixed the retry loop for real",
        );
        let id = seed_deterministic_dream_relation(&storage, "ep-det-a", "ep-det-b");
        let candidate = QueuedRelation {
            id,
            ep_a: "ep-det-a".to_string(),
            ep_b: "ep-det-b".to_string(),
            ..mk_candidate("created_at_fallback", None)
        };
        let a = mk_facts("2020-01-01T00:00:00Z", "fix the retry loop");
        let b = mk_facts("2020-02-01T00:00:00Z", "fixed the retry loop for real");

        let outcome = storage
            .with_connection(|conn| {
                let mut cache = OidCache::new();
                verify_and_apply_deterministic(
                    conn,
                    &candidate,
                    &a,
                    &b,
                    Relation::ReplacedBy,
                    &mut cache,
                )
            })
            .unwrap();
        assert!(matches!(outcome, VerifyOutcome::Passed));

        let (tier, quote_a, quote_b): (String, String, String) = storage
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT tier, quote_a, quote_b FROM dream_relations WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(tier, TIER_CEILING);
        assert_eq!(
            quote_a, "fix the retry loop",
            "must be B's wired, verbatim quote_a — never empty"
        );
        assert_eq!(
            quote_b, "fixed the retry loop for real",
            "must be B's wired, verbatim quote_b — never empty"
        );
        // (`a`/`b` above are only OID-check placeholders for this call;
        // the quotes themselves are pulled straight from `episode_index`
        // by `fill_pair_quotes`, byte-verified against it before this
        // function ever sees them — see `verify_quote`.)
    }

    #[test]
    fn verify_and_apply_deterministic_rejects_an_empty_field_instead_of_a_false_pointer_promotion()
    {
        let storage = open();
        // ep-det-c's `request` is empty: `extract_quote` correctly returns a
        // Pointer for it (never a fabricated non-empty quote) — but this
        // deterministic path must NOT accept that Pointer as satisfying its
        // own non-empty-quote contract; it must discard instead.
        insert_episode_for_quotes(
            &storage,
            "ep-det-c",
            "2020-01-01T00:00:00Z",
            "",
            "did some prep",
        );
        insert_episode_for_quotes(
            &storage,
            "ep-det-d",
            "2020-02-01T00:00:00Z",
            "some later ask",
            "fixed the retry loop for real",
        );
        let id = seed_deterministic_dream_relation(&storage, "ep-det-c", "ep-det-d");
        let candidate = QueuedRelation {
            id,
            ep_a: "ep-det-c".to_string(),
            ep_b: "ep-det-d".to_string(),
            ..mk_candidate("created_at_fallback", None)
        };
        let a = mk_facts("2020-01-01T00:00:00Z", "");
        let b = mk_facts("2020-02-01T00:00:00Z", "fixed the retry loop for real");

        let outcome = storage
            .with_connection(|conn| {
                let mut cache = OidCache::new();
                verify_and_apply_deterministic(
                    conn,
                    &candidate,
                    &a,
                    &b,
                    Relation::ReplacedBy,
                    &mut cache,
                )
            })
            .unwrap();
        assert!(matches!(outcome, VerifyOutcome::Failed("empty_quote_a")));

        let (tier, status): (String, String) = storage
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT tier, status FROM dream_relations WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(
            tier, "unverified",
            "an empty-field Pointer must never promote"
        );
        assert_eq!(status, "archived");
    }

    // -----------------------------------------------------------------
    // B: promote_relation archives-as-duplicate instead of throwing on the
    // UNIQUE(project, ep_a, ep_b, relation) index.
    // -----------------------------------------------------------------

    #[test]
    fn promote_relation_archives_as_duplicate_when_a_sibling_row_already_holds_the_relation() {
        let storage = open();
        // A sibling row for the SAME (project, ep_a, ep_b) already promoted
        // to 'replaced_by' by an earlier adjudication (or a twin-hypothesis
        // row from a pre-P3 run).
        let id1 = storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO dream_relations
                        (project, ep_a, ep_b, relation, generator, topic_key, tier, status)
                     VALUES ('p', 'a', 'b', 'replaced_by', 'ledger', 'symbol:foo', 'witnessed', 'queued')",
                    [],
                )?;
                Ok(conn.last_insert_rowid())
            })
            .unwrap();
        let id2 = storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO dream_relations
                        (project, ep_a, ep_b, relation, generator, topic_key, tier, status)
                     VALUES ('p', 'a', 'b', 'extended_by', 'relapse', 'symbol:foo', 'unverified', 'queued')",
                    [],
                )?;
                Ok(conn.last_insert_rowid())
            })
            .unwrap();

        let candidate = QueuedRelation {
            id: id2,
            ..mk_candidate("created_at_fallback", None)
        };
        let a = mk_facts("2020-01-01T00:00:00Z", "we always retry on timeout errors");
        let b = mk_facts("2020-02-01T00:00:00Z", "we now fail fast on timeout errors");
        // decide_relation would land this candidate on the SAME relation
        // (replaced_by) the sibling already holds.
        let verdict = mk_verdict(
            "we always retry on timeout errors",
            "we now fail fast on timeout errors",
            vec![],
        );

        let outcome = storage
            .with_connection(|conn| {
                let mut cache = OidCache::new();
                verify_and_apply(
                    conn,
                    &candidate,
                    &a,
                    &[],
                    &b,
                    &[],
                    Relation::ReplacedBy,
                    &verdict,
                    &mut cache,
                    "{}",
                )
            })
            .unwrap();
        assert!(
            matches!(outcome, VerifyOutcome::Failed("duplicate_story")),
            "must archive-as-duplicate, never let the UNIQUE index throw and abort the run"
        );

        let (status2, relation2, tier2): (String, String, String) = storage
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT status, relation, tier FROM dream_relations WHERE id = ?1",
                    [id2],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(status2, "archived");
        assert_eq!(
            relation2, "extended_by",
            "the archived row's stored relation must be left untouched, never overwritten en route to being archived"
        );
        assert_eq!(tier2, "unverified");

        // The sibling row is completely untouched.
        let (status1, tier1): (String, String) = storage
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT status, tier FROM dream_relations WHERE id = ?1",
                    [id1],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(status1, "queued");
        assert_eq!(tier1, "witnessed");

        let discards: i64 = storage
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM backfill_discards WHERE reason = 'duplicate_story'",
                    [],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(discards, 1);
    }

    // -----------------------------------------------------------------
    // P7 (F7/F8): reason split + insertion-noise tolerance.
    // -----------------------------------------------------------------

    #[test]
    fn verify_and_apply_discards_an_empty_quote_with_its_own_reason() {
        let storage = open();
        let id = seed_dream_relation(&storage);
        let candidate = QueuedRelation {
            id,
            ..mk_candidate("created_at_fallback", None)
        };
        let a = mk_facts("2020-01-01T00:00:00Z", "we always retry on timeout errors");
        let b = mk_facts("2020-02-01T00:00:00Z", "we now fail fast on timeout errors");
        let verdict = mk_verdict("", "we now fail fast on timeout errors", vec![]);

        let outcome = storage
            .with_connection(|conn| {
                let mut cache = OidCache::new();
                verify_and_apply(
                    conn,
                    &candidate,
                    &a,
                    &[],
                    &b,
                    &[],
                    Relation::ReplacedBy,
                    &verdict,
                    &mut cache,
                    "{}",
                )
            })
            .unwrap();
        assert!(matches!(outcome, VerifyOutcome::Failed("empty_quote_a")));
    }

    #[test]
    fn quote_matches_forgives_insertion_noise_a_fixed_window_jaccard_would_false_discard() {
        // T4/F7: the source interleaves three extra tokens ("also", "in",
        // "src/parser.rs") between the quote's real words -- a fixed-length
        // windowed-set-Jaccard matcher discards this (best 8-token window
        // shares only 6/10 = 0.60 < its own threshold); the expandable
        // content-coverage window must keep it.
        let quote = "patched parse_frame to handle the new heartbeat frame";
        let source = "also patched parse_frame in src/parser.rs to handle the new heartbeat frame via a callback shim";
        assert!(quote_matches(quote, source));
    }

    #[test]
    fn quote_matches_rejects_a_stopword_only_quote() {
        // T4 adversarial case: every token is a stopword -- must never pass
        // just because a naive set-based matcher would find them all.
        assert!(!quote_matches(
            "the to in and a of the",
            "the to in and a of the here"
        ));
    }

    #[test]
    fn quote_matches_rejects_scrambled_token_order() {
        // T4 adversarial case: the right content tokens, wrong order --
        // the LCS order gate must kill this even though set coverage is 1.0.
        let quote = "patched parse_frame to handle the new heartbeat frame";
        let scrambled = "frame heartbeat new the handle to parse_frame patched";
        assert!(!quote_matches(quote, scrambled));
    }

    #[test]
    fn quote_found_in_other_episode_yields_the_misattributed_reason() {
        let storage = open();
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO episode_index
                        (episode_id, session_id, project, ts, outcome, request, completed,
                         next_steps, blockers, todo_count, files_json, anchors_json)
                     VALUES ('c', 'sc', 'p', '2020-03-01T00:00:00Z', 'completed',
                             'unrelated request', 'Rewrote src/config.rs entirely',
                             NULL, NULL, 0, '[]', '[]')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let id = seed_dream_relation(&storage);
        let candidate = QueuedRelation {
            id,
            ..mk_candidate("created_at_fallback", None)
        };
        let a = mk_facts("2020-01-01T00:00:00Z", "we always retry on timeout errors");
        let b = mk_facts("2020-02-01T00:00:00Z", "we now fail fast on timeout errors");
        // The quote is real text -- just attributed to the wrong episode
        // (it actually lives in episode 'c', not the claimed 'a').
        let verdict = mk_verdict(
            "Rewrote src/config.rs entirely",
            "we now fail fast on timeout errors",
            vec![],
        );

        let outcome = storage
            .with_connection(|conn| {
                let mut cache = OidCache::new();
                verify_and_apply(
                    conn,
                    &candidate,
                    &a,
                    &[],
                    &b,
                    &[],
                    Relation::ReplacedBy,
                    &verdict,
                    &mut cache,
                    "{}",
                )
            })
            .unwrap();
        assert!(matches!(
            outcome,
            VerifyOutcome::Failed("misattributed_quote_a")
        ));
    }

    // -----------------------------------------------------------------
    // P8 (F6): ts_ordered no longer delegates to SQLite julianday.
    // -----------------------------------------------------------------

    #[test]
    fn ts_ordered_handles_an_unparseable_timestamp_as_discard_safe_false() {
        let storage = open();
        storage
            .with_connection(|conn| {
                assert!(!ts_ordered(conn, "not a timestamp", "2020-01-01T00:00:00Z").unwrap());
                assert!(!ts_ordered(conn, "2020-01-01T00:00:00Z", "also not one").unwrap());
                assert!(ts_ordered(conn, "2020-01-01T00:00:00Z", "2020-02-01T00:00:00Z").unwrap());
                assert!(!ts_ordered(conn, "2020-02-01T00:00:00Z", "2020-01-01T00:00:00Z").unwrap());
                Ok(())
            })
            .unwrap();
    }

    // -----------------------------------------------------------------
    // P9 (F9): oid_in_ledger is project-scoped.
    // -----------------------------------------------------------------

    #[test]
    fn verify_and_apply_rejects_an_oid_that_only_exists_in_another_projects_ledger() {
        use crate::storage::witness_ledger::{insert_witness, WitnessLedgerRow};

        let storage = open();
        let id = seed_dream_relation(&storage); // project = 'p'
        storage
            .with_connection(|conn| {
                insert_witness(
                    conn,
                    &WitnessLedgerRow {
                        id: 0,
                        project: "other-project".to_string(),
                        file: "a.rs".to_string(),
                        symbol: Some("foo".to_string()),
                        span_start: None,
                        span_end: None,
                        stamp: "b3:aaa".to_string(),
                        tier: "committed".to_string(),
                        at_oid: Some("crossprojoid".to_string()),
                        source_kind: "backfill".to_string(),
                        source_id: None,
                    },
                )
            })
            .unwrap();

        let candidate = QueuedRelation {
            id,
            ..mk_candidate("created_at_fallback", None)
        };
        let a = mk_facts("2020-01-01T00:00:00Z", "we always retry on timeout errors");
        let b = mk_facts("2020-02-01T00:00:00Z", "we now fail fast on timeout errors");
        let verdict = mk_verdict(
            "we always retry on timeout errors",
            "we now fail fast on timeout errors",
            vec!["crossprojoid".to_string()],
        );

        let outcome = storage
            .with_connection(|conn| {
                let mut cache = OidCache::new();
                verify_and_apply(
                    conn,
                    &candidate,
                    &a,
                    &[],
                    &b,
                    &[],
                    Relation::ReplacedBy,
                    &verdict,
                    &mut cache,
                    "{}",
                )
            })
            .unwrap();
        assert!(
            matches!(outcome, VerifyOutcome::Failed("fake_oid")),
            "an OID recorded only in ANOTHER project's ledger must not validate this project's citation"
        );
    }
}
