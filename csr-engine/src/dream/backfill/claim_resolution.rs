//! B: deterministic claim resolution + the verbatim quote-extraction fix
//! (dream backfill pass 1).
//!
//! # Scope note — reconciling the design against the shipped pipeline
//!
//! The design this module was drafted against (`.plans/dream-backfill-design.md`
//! plus two independent model drafts of it) describes a "Stage 4.5" that
//! classifies every promoted relation and re-derives its quotes
//! deterministically, sitting between Stage 4 (adjudicate) and Stage 5
//! (verify). **That stage does not exist in the shipped pipeline.** The
//! pipeline that actually ships (`adjudicate` → `verify` → `compose`) has
//! an LLM produce `quote_a`/`quote_b`/the relation claim at Stage 4, and
//! [`super::verify::quote_verified`] deterministically re-checks the LLM's
//! claimed quotes are real verbatim substrings of the episode (discarding
//! the candidate on `fabricated_quote_a`/`fabricated_quote_b` otherwise) —
//! Stage 5 already carries a materially similar defense to what this
//! module's "thin-quote fix" describes.
//!
//! Given the hard rule for this pass (deterministic, no LLM on the gate
//! path, additive-only — never rewrite Stage 4/5/6), this module is built
//! as a **standalone, fully deterministic, zero-LLM** capability, tested on
//! its own, with one concrete wiring point: [`fill_quotes_for_pair`] takes
//! a promoted [`super::pairs::PairCandidate`] and returns a non-empty,
//! byte-verified `(quote_a, quote_b)` pair pulled straight from
//! `episode_index` — usable as a deterministic quote source for a future
//! Stage 4.5, or as an alternative to a fabricated LLM quote, without this
//! pass reaching into `adjudicate.rs`/`verify.rs`'s control flow.
//!
//! One more reconciliation: `episode_index` carries no single raw-transcript
//! "content" blob (the design assumed one) — only its own structured
//! narrative fields (`request`, `completed`, `outcome`, `blockers`,
//! `next_steps`). [`fill_quotes_for_pair`] uses the concatenation of those
//! fields as the "content" haystack a quote is verified against; see its
//! doc comment for what that does and doesn't prove.
//!
//! Zero LLM calls anywhere in this module.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection};

use super::death_time::{git_at, git_show_bytes};
use super::pairs::PairCandidate;
use crate::extraction::repo_root::repo_root_for_file;

// ---------------------------------------------------------------------
// Tiered claim classifier
// ---------------------------------------------------------------------

/// Deterministic classification of a relapse's re-touch, precedence
/// exact > cleanup > partial > migration > unclear. Every variant is a
/// falsifiable, receipt-backed claim about how the pre-funeral body relates
/// to the post-touch body — never a paraphrase, never an LLM guess.
#[derive(Debug, Clone, PartialEq)]
pub enum ClaimClass {
    /// Post-touch content is byte-identical to the pre-funeral body: the
    /// funeral was simply wrong, not a relapse at all.
    ResurrectionExact,
    /// Post-touch content overlaps the pre-funeral body at or above the
    /// pre-registered line-Jaccard threshold, and overlaps the
    /// funeral-time body less: the old approach was re-adopted.
    ResurrectionPartial { jaccard: f64 },
    /// Bookkeeping, not a real re-adoption: whitespace-only or
    /// token-identical reshuffle between funeral-time and post-touch.
    Cleanup { reason: &'static str },
    /// Neither resurrection nor cleanup, but the symbol's caller count
    /// shifted meaningfully between funeral and post-touch — moved, not
    /// resurrected.
    Migration { caller_delta: i64 },
    /// None of the above tiers fired, or a required span/oid could not be
    /// resolved. Never composed into a claim.
    Unclear,
}

impl ClaimClass {
    /// Whether this classification must be demoted (excluded from
    /// composing into a claim) rather than surfaced.
    pub fn demoted(&self) -> bool {
        matches!(self, ClaimClass::Cleanup { .. } | ClaimClass::Unclear)
    }
}

fn squash_whitespace(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn token_multiset(s: &str) -> Vec<String> {
    let mut toks: Vec<String> = s
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|t| t.len() > 1)
        .map(|t| t.to_ascii_lowercase())
        .collect();
    toks.sort();
    toks
}

/// Trimmed, non-empty lines, PRESERVING ORDER AND DUPLICATES — the sequence
/// [`line_similarity`] compares. Deliberately not a `HashSet`: two bodies
/// sharing the exact same lines in a DIFFERENT order (a reordered function)
/// must NOT collapse to the same value a `HashSet`-based Jaccard would give
/// them (Codex review pass 1, finding #6 — "a reordered body can score 1.0
/// as a partial resurrection").
fn line_seq(s: &str) -> Vec<&str> {
    s.lines().map(str::trim).filter(|l| !l.is_empty()).collect()
}

/// Sequence-aware body similarity: `2 * LCS(a, b) / (len(a) + len(b))`
/// (the same normalization `difflib.SequenceMatcher.ratio` uses), computed
/// over LINES, not characters. Unlike a set/multiset Jaccard, the Longest
/// Common Subsequence is order-sensitive — a body reordered but otherwise
/// unchanged scores well BELOW 1.0 here (its LCS is at most the length of
/// its longest untouched run, not every line), while a body that is
/// genuinely re-adopted with only a handful of insertions/edits (the same
/// relative ORDER preserved) still scores high. `1.0` only when both sides
/// are truly identical line sequences (order and content both), including
/// the "both empty" edge case.
fn line_similarity(a: &str, b: &str) -> f64 {
    let (sa, sb) = (line_seq(a), line_seq(b));
    if sa.is_empty() && sb.is_empty() {
        return 1.0;
    }
    if sa.is_empty() || sb.is_empty() {
        return 0.0;
    }
    let lcs = lcs_len(&sa, &sb);
    (2.0 * lcs as f64) / (sa.len() + sb.len()) as f64
}

/// Longest Common Subsequence length over two line slices — standard O(n*m)
/// DP. `pre`/`at_death`/`post` spans are single extracted symbol bodies
/// (typically tens of lines), so this is cheap in practice; this module
/// never runs it over whole files.
fn lcs_len(a: &[&str], b: &[&str]) -> usize {
    let (n, m) = (a.len(), b.len());
    let mut prev = vec![0usize; m + 1];
    let mut cur = vec![0usize; m + 1];
    for i in 1..=n {
        for j in 1..=m {
            cur[j] = if a[i - 1] == b[j - 1] {
                prev[j - 1] + 1
            } else {
                prev[j].max(cur[j - 1])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[m]
}

/// The pre-registered "re-adopted, not superseded" threshold.
const RESURRECTION_PARTIAL_THRESHOLD: f64 = 0.6;
/// Below this many net caller references, a shift isn't confidently a
/// migration signal — it stays `Unclear` rather than a low-confidence guess.
const MIGRATION_DELTA_MIN: i64 = 1;

/// `git grep -I -w -c <symbol> <oid> -- <pathspec...>` matching-LINE count
/// (git's own `-c` semantics: lines that match, not raw occurrences — two
/// calls on one line still count as one) — a caller-graph churn proxy, not
/// a real call-graph. `pathspec` narrows the search (typically the file's
/// own extension) to keep the count meaningful across a large repo.
///
/// Returns `None` on a REAL git failure (spawn failure, or `git grep`
/// exiting >= 2 — an unresolvable `oid`, a bad pathspec, an invalid
/// pattern) — distinct from a genuine zero-match result, which `git grep
/// -c` reports via exit code 1 (not an error) and this function reports as
/// `Some(0)`. Codex review pass 1, finding #6: the previous version
/// collapsed BOTH cases to `0`, so one side's real git error plus the
/// other side's real zero-match count could manufacture a nonzero,
/// entirely spurious [`ClaimClass::Migration`] delta.
fn caller_occurrences(repo_root: &str, oid: &str, symbol: &str, pathspec: &str) -> Option<i64> {
    let output = git_at(repo_root)
        .arg("grep")
        .arg("-I")
        .arg("-w")
        .arg("-c")
        .arg(symbol)
        .arg(oid)
        .arg("--")
        .arg(pathspec)
        .output()
        .ok()?;
    match output.status.code() {
        Some(0) | Some(1) => {}
        _ => return None, // real failure: unresolvable oid, bad pathspec, etc.
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|l| l.rsplit(':').next()?.trim().parse::<i64>().ok())
            .sum(),
    )
}

/// Tiered classification of one relapse's re-touch. `pre`/`at_death`/`post`
/// are the symbol's span text (already extracted by the caller) at the
/// witness's own `at_oid`, the bisected death commit, and the later
/// post-touch commit, respectively. `repo_root`/`symbol`/`pathspec` feed
/// [`caller_occurrences`] for the migration tier only (called lazily, since
/// it shells out).
#[allow(clippy::too_many_arguments)]
pub fn classify_relapse(
    pre: &str,
    at_death: &str,
    post: &str,
    repo_root: &str,
    death_oid: &str,
    post_oid: &str,
    symbol: &str,
    pathspec: &str,
) -> ClaimClass {
    // Codex review pass 1, finding #6: `ResurrectionExact` is documented as
    // BYTE-IDENTICAL — a whitespace-stripped-equal comparison is a
    // materially weaker claim (re-indented, differently-wrapped code would
    // pass) and must not carry this label. The whitespace-insensitive
    // comparison still has a real job below, just under `Cleanup`.
    if post == pre {
        return ClaimClass::ResurrectionExact;
    }
    if squash_whitespace(post) == squash_whitespace(pre) {
        return ClaimClass::Cleanup {
            reason: "whitespace/format only vs. the pre-funeral body",
        };
    }
    if squash_whitespace(post) == squash_whitespace(at_death) {
        return ClaimClass::Cleanup {
            reason: "whitespace/format only",
        };
    }
    if token_multiset(post) == token_multiset(at_death) {
        return ClaimClass::Cleanup {
            reason: "token-identical reshuffle",
        };
    }

    // Codex review pass 1, finding #6: sequence-aware (LCS-based), not a
    // set/multiset Jaccard — a body whose lines are merely REORDERED must
    // not score a spurious 1.0 here (see `line_similarity`'s doc comment).
    let sim_pre = line_similarity(post, pre);
    let sim_death = line_similarity(post, at_death);
    if sim_pre >= RESURRECTION_PARTIAL_THRESHOLD && sim_pre > sim_death {
        return ClaimClass::ResurrectionPartial { jaccard: sim_pre };
    }

    // Codex review pass 1, finding #6: a real git failure on EITHER side
    // must never silently become "0 callers" — that can manufacture a
    // spurious `Migration` delta from one side's error and the other's
    // genuine count. Both sides must resolve before a delta is trusted.
    match (
        caller_occurrences(repo_root, post_oid, symbol, pathspec),
        caller_occurrences(repo_root, death_oid, symbol, pathspec),
    ) {
        (Some(post_count), Some(death_count)) => {
            let delta = post_count - death_count;
            if delta.abs() >= MIGRATION_DELTA_MIN {
                return ClaimClass::Migration {
                    caller_delta: delta,
                };
            }
        }
        _ => {
            // Caller-graph churn is unresolvable (bad oid/pathspec/no git):
            // no confident basis for a Migration claim, but that is not the
            // same as "no migration" either — stays Unclear either way.
        }
    }
    ClaimClass::Unclear
}

/// Zombie recurrence: `git log -S<needle> <death_oid>..HEAD` over code
/// paths, excluding `exclude_oids` (typically the successor's own landing
/// commit — a migration is not a re-proposal of the buried approach).
/// `needle` should be a distinctive line from the dead span (the caller's
/// job — this function does not pick one). Empty result = tombstone
/// (no recurrence); non-empty = the receipt OIDs of every re-proposal.
pub fn zombie_recurrence(
    repo_root: &str,
    needle: &str,
    death_oid: &str,
    head_oid: &str,
    pathspec: &str,
    exclude_oids: &HashSet<String>,
) -> Vec<String> {
    if needle.trim().is_empty() {
        return Vec::new();
    }
    let range = format!("{death_oid}..{head_oid}");
    let pick = format!("-S{needle}");
    let output = git_at(repo_root)
        .arg("log")
        .arg("--format=%H")
        .arg(&pick)
        .arg(&range)
        .arg("--")
        .arg(pathspec)
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .filter(|oid| !exclude_oids.contains(oid))
        .collect()
}

/// Extract the symbol's span text at `oid` from `file`, resolving `file`'s
/// repo automatically. `None` when the file/commit is unresolvable, or the
/// requested lines don't exist there (renamed/deleted) — a real fact, not
/// an error to swallow, so callers treat `None` as "cannot classify".
pub fn span_text_at(
    file: &str,
    oid: &str,
    span_start_0based: Option<i64>,
    span_end_0based: Option<i64>,
) -> Option<String> {
    let repo_root = repo_root_for_file(file)?;
    let root =
        std::fs::canonicalize(&repo_root).unwrap_or_else(|_| Path::new(&repo_root).to_path_buf());
    let abs = std::fs::canonicalize(file).unwrap_or_else(|_| Path::new(file).to_path_buf());
    let relpath = abs.strip_prefix(&root).ok()?;
    let relpath = relpath
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    let bytes = git_show_bytes(&repo_root, oid, &relpath)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    match (span_start_0based, span_end_0based) {
        (Some(s), Some(e)) if s >= 0 && e >= s => {
            let lines: Vec<&str> = text.lines().collect();
            let (s, e) = (s as usize, e as usize);
            if e >= lines.len() {
                return None;
            }
            Some(lines[s..=e].join("\n"))
        }
        _ => Some(text),
    }
}

// ---------------------------------------------------------------------
// The thin-quote fix: deterministic, verbatim, byte-offset-receipted
// ---------------------------------------------------------------------

/// A quoted slot: either a verified verbatim extract with byte offsets into
/// the `content` it was pulled from, or an explicit pointer when nothing
/// verbatim could be located. There is no third state — a paraphrase is
/// never a valid [`QuoteSlot`].
#[derive(Debug, Clone, PartialEq)]
pub enum QuoteSlot {
    Verbatim {
        episode_id: String,
        text: String,
        byte_start: usize,
        byte_end: usize,
    },
    Pointer {
        episode_id: String,
    },
}

impl QuoteSlot {
    pub fn is_verbatim(&self) -> bool {
        matches!(self, QuoteSlot::Verbatim { .. })
    }
}

/// The thin-quote fix (B). Locate `field`'s text VERBATIM inside `content`:
/// first an exact substring match (the common case — `field` usually IS a
/// stored fragment of `content`); failing that, a word-overlap fallback
/// that still returns only a real, byte-exact line of `content` (never a
/// synthesized string) when at least 60% of `field`'s significant words
/// appear in that line. No match at all -> [`QuoteSlot::Pointer`]. There is
/// no LLM anywhere in this function, and no path returns text that isn't a
/// literal slice of `content`.
pub fn extract_quote(episode_id: &str, content: &str, field: &str) -> QuoteSlot {
    let needle = field.trim();
    if needle.is_empty() {
        return QuoteSlot::Pointer {
            episode_id: episode_id.to_string(),
        };
    }
    if let Some(pos) = content.find(needle) {
        return QuoteSlot::Verbatim {
            episode_id: episode_id.to_string(),
            text: needle.to_string(),
            byte_start: pos,
            byte_end: pos + needle.len(),
        };
    }

    let norm = |w: &str| {
        w.trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
    };
    let want: HashSet<String> = needle
        .split_whitespace()
        .map(norm)
        .filter(|w| w.len() > 2)
        .collect();
    if want.is_empty() {
        return QuoteSlot::Pointer {
            episode_id: episode_id.to_string(),
        };
    }

    let mut best: Option<(f64, usize, usize)> = None;
    let mut offset = 0usize;
    for line in content.split_inclusive('\n') {
        let bare = line.strip_suffix('\n').unwrap_or(line);
        let got: HashSet<String> = bare.split_whitespace().map(norm).collect();
        if !got.is_empty() {
            let score = want.intersection(&got).count() as f64 / want.len() as f64;
            if score >= 0.6 && best.as_ref().is_none_or(|(b, _, _)| score > *b) {
                best = Some((score, offset, offset + bare.len()));
            }
        }
        offset += line.len();
    }
    match best {
        Some((_, s, e)) => QuoteSlot::Verbatim {
            episode_id: episode_id.to_string(),
            text: content[s..e].to_string(),
            byte_start: s,
            byte_end: e,
        },
        None => QuoteSlot::Pointer {
            episode_id: episode_id.to_string(),
        },
    }
}

/// Re-verify one [`QuoteSlot`] against `content` (the exact string it was
/// extracted from, or a freshly re-fetched equivalent) — fails closed: a
/// [`QuoteSlot::Pointer`] always passes (nothing to verify); a
/// [`QuoteSlot::Verbatim`] must byte-match at its own recorded offsets, or
/// this returns an error naming why.
pub fn verify_quote(content: &str, quote: &QuoteSlot) -> Result<()> {
    match quote {
        QuoteSlot::Pointer { .. } => Ok(()),
        QuoteSlot::Verbatim {
            episode_id,
            text,
            byte_start,
            byte_end,
        } => {
            let slice = content
                .get(*byte_start..*byte_end)
                .ok_or_else(|| anyhow!("quote offsets out of range for episode {episode_id}"))?;
            if slice != text {
                anyhow::bail!(
                    "quote is not verbatim in episode {episode_id} at its recorded offsets"
                );
            }
            Ok(())
        }
    }
}

/// Compose-stage guard (B): every quoted slot in `quotes` must be a real,
/// byte-verified extract of `content`, or an explicit pointer — never
/// anything else. Fails on the first violation, naming which slot failed.
/// `content` is the SAME string `extract_quote` was called with for each
/// listed slot (byte offsets are meaningless across different strings).
pub fn assert_composable(content: &str, quotes: &[&QuoteSlot]) -> Result<()> {
    for q in quotes {
        verify_quote(content, q)?;
    }
    Ok(())
}

/// [`assert_composable`] across a batch of `(content, quote)` pairs, for
/// the common case where `quote_a`/`quote_b` were pulled from two
/// different episodes' content and so need two different haystacks.
pub fn assert_verified_quotes(pairs: &[(&str, &QuoteSlot)]) -> Result<()> {
    for (content, quote) in pairs {
        verify_quote(content, quote)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Wiring: promoted pair -> non-empty quote_a/quote_b (real episode_index)
// ---------------------------------------------------------------------

struct EpisodeText {
    request: String,
    completed: String,
    outcome: String,
    blockers: Option<String>,
    next_steps: Option<String>,
}

impl EpisodeText {
    /// `episode_index` carries no single raw-transcript "content" column
    /// (see module doc) — this concatenation of its own structured
    /// narrative fields is the closest available haystack. A quote
    /// extracted against it therefore proves "this text is a real,
    /// unmodified fragment of what THIS episode itself recorded", not "this
    /// text appears in the underlying transcript" (a strictly weaker but
    /// still real and re-verifiable claim).
    fn haystack(&self) -> String {
        [
            self.request.as_str(),
            self.completed.as_str(),
            self.outcome.as_str(),
            self.blockers.as_deref().unwrap_or(""),
            self.next_steps.as_deref().unwrap_or(""),
        ]
        .join("\n")
    }
}

fn load_episode_text(conn: &Connection, episode_id: &str) -> Result<EpisodeText> {
    conn.query_row(
        "SELECT request, completed, outcome, blockers, next_steps
         FROM episode_index WHERE episode_id = ?1",
        params![episode_id],
        |r| {
            Ok(EpisodeText {
                request: r.get(0)?,
                completed: r.get(1)?,
                outcome: r.get(2)?,
                blockers: r.get(3)?,
                next_steps: r.get(4)?,
            })
        },
    )
    .map_err(|e| anyhow!("loading episode {episode_id} for quote extraction: {e}"))
}

/// B's wiring point: given a promoted pair's two episode ids, return
/// verified, non-empty `(quote_a, quote_b)` — `quote_a` from the
/// before-episode's own `request` (what it set out to do), `quote_b` from
/// the after-episode's own `completed` (what it actually did), each
/// verbatim-extracted (never LLM-authored) from that episode's own
/// narrative fields via [`extract_quote`], and re-verified via
/// [`verify_quote`] before returning.
pub fn fill_pair_quotes(
    conn: &Connection,
    ep_a_id: &str,
    ep_b_id: &str,
) -> Result<(QuoteSlot, QuoteSlot)> {
    let a = load_episode_text(conn, ep_a_id)?;
    let b = load_episode_text(conn, ep_b_id)?;
    let hay_a = a.haystack();
    let hay_b = b.haystack();
    let quote_a = extract_quote(ep_a_id, &hay_a, &a.request);
    let quote_b = extract_quote(ep_b_id, &hay_b, &b.completed);
    verify_quote(&hay_a, &quote_a)?;
    verify_quote(&hay_b, &quote_b)?;
    Ok((quote_a, quote_b))
}

/// [`fill_pair_quotes`] over a real [`PairCandidate`] (`ep_a`/`ep_b`
/// already carry the episode ids) — the literal "promoted relapse/ledger
/// pairs get a non-empty quote_a/quote_b" wiring this module exists to
/// provide.
pub fn fill_quotes_for_pair(
    conn: &Connection,
    pair: &PairCandidate,
) -> Result<(QuoteSlot, QuoteSlot)> {
    fill_pair_quotes(conn, &pair.ep_a, &pair.ep_b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    // -----------------------------------------------------------------
    // extract_quote / verify_quote / assert_composable
    // -----------------------------------------------------------------

    #[test]
    fn extract_quote_finds_the_verbatim_exact_substring_with_correct_offsets() {
        let content = "line one\nwe always retry on timeout errors\nline three\n";
        let field = "we always retry on timeout errors";
        let q = extract_quote("ep-1", content, field);
        let QuoteSlot::Verbatim {
            text,
            byte_start,
            byte_end,
            episode_id,
        } = q
        else {
            panic!("expected a verbatim extract");
        };
        assert_eq!(episode_id, "ep-1");
        assert_eq!(text, field);
        assert_eq!(&content[byte_start..byte_end], field);
    }

    #[test]
    fn extract_quote_falls_back_to_a_word_overlap_line_when_no_exact_substring() {
        // The field text has the same significant words as a real line in
        // content, just reordered -- no exact substring exists, but the
        // fallback must still return a REAL line of content, never a
        // synthesized string.
        let content = "before\nretries during timeout errors always happen sometimes\nafter\n";
        let field = "timeout errors always happen during retries";
        let q = extract_quote("ep-2", content, field);
        let QuoteSlot::Verbatim {
            text,
            byte_start,
            byte_end,
            ..
        } = q
        else {
            panic!("expected a verbatim fallback extract");
        };
        assert_eq!(
            text,
            "retries during timeout errors always happen sometimes"
        );
        assert_eq!(&content[byte_start..byte_end], text);
    }

    #[test]
    fn extract_quote_returns_a_pointer_when_nothing_verbatim_matches() {
        let content = "completely unrelated content here\n";
        let q = extract_quote("ep-3", content, "something entirely different words");
        assert_eq!(
            q,
            QuoteSlot::Pointer {
                episode_id: "ep-3".to_string()
            }
        );
    }

    #[test]
    fn extract_quote_empty_field_is_a_pointer_not_a_fabrication() {
        let q = extract_quote("ep-4", "some content", "");
        assert!(matches!(q, QuoteSlot::Pointer { .. }));
    }

    #[test]
    fn verify_quote_rejects_a_tampered_offset() {
        let content = "the real verbatim line\n";
        let mut q = extract_quote("ep-5", content, "the real verbatim line");
        if let QuoteSlot::Verbatim { byte_start, .. } = &mut q {
            *byte_start += 1; // corrupt it
        }
        assert!(verify_quote(content, &q).is_err());
    }

    #[test]
    fn verify_quote_accepts_a_pointer_unconditionally() {
        let q = QuoteSlot::Pointer {
            episode_id: "ep-6".to_string(),
        };
        assert!(verify_quote("anything", &q).is_ok());
    }

    #[test]
    fn assert_composable_rejects_a_non_extract_quote() {
        let content = "actual content";
        let fabricated = QuoteSlot::Verbatim {
            episode_id: "ep-7".to_string(),
            text: "the model made this up".to_string(),
            byte_start: 0,
            byte_end: 5,
        };
        let err = assert_composable(content, &[&fabricated]).unwrap_err();
        assert!(err.to_string().contains("ep-7"));
    }

    #[test]
    fn assert_composable_accepts_a_real_extract() {
        let content = "the real verbatim line\n";
        let q = extract_quote("ep-8", content, "the real verbatim line");
        assert!(assert_composable(content, &[&q]).is_ok());
    }

    // -----------------------------------------------------------------
    // fill_pair_quotes / fill_quotes_for_pair: wired against real
    // episode_index rows.
    // -----------------------------------------------------------------

    fn open() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::storage::migrations::run(&conn).unwrap();
        conn
    }

    fn episode_json(request: &str, completed: &str) -> String {
        format!(
            r#"{{
                "schema": "v2",
                "session_id": "sess",
                "project": "proj",
                "timestamp": "2020-01-01T00:00:00Z",
                "request": "{request}",
                "investigated": [],
                "completed": "{completed}",
                "next_steps": null,
                "blockers": null,
                "outcome": "completed",
                "error_signatures": [],
                "tools_used": [],
                "files_modified": [],
                "message_count": 1,
                "duration_minutes": 1,
                "todos": [],
                "approved_plan": null,
                "prev_episode_id": null,
                "anchors": []
            }}"#
        )
    }

    fn insert_episode(conn: &Connection, id: &str, request: &str, completed: &str) {
        conn.execute(
            "INSERT INTO reflections (id, content, tags, timestamp) VALUES (?1, ?2, '[]', ?3)",
            params![id, episode_json(request, completed), "2020-01-01T00:00:00Z"],
        )
        .unwrap();
        crate::storage::dream_backfill::materialize_episode_index(conn).unwrap();
    }

    #[test]
    fn fill_pair_quotes_returns_non_empty_verified_quotes_for_a_real_pair() {
        let conn = open();
        insert_episode(&conn, "ep-a", "fix the retry loop", "fixed the retry loop");
        insert_episode(&conn, "ep-b", "add a timeout", "added a timeout guard");

        let (quote_a, quote_b) = fill_pair_quotes(&conn, "ep-a", "ep-b").unwrap();
        assert!(
            quote_a.is_verbatim(),
            "quote_a must be non-empty and verbatim"
        );
        assert!(
            quote_b.is_verbatim(),
            "quote_b must be non-empty and verbatim"
        );

        let QuoteSlot::Verbatim { text: text_a, .. } = &quote_a else {
            unreachable!()
        };
        let QuoteSlot::Verbatim { text: text_b, .. } = &quote_b else {
            unreachable!()
        };
        assert_eq!(text_a, "fix the retry loop");
        assert_eq!(text_b, "added a timeout guard");
    }

    fn seed_pair_candidate(ep_a: &str, ep_b: &str) -> PairCandidate {
        use super::super::pairs::{Generator, OidProvenance, PairReceipt, Relation};
        PairCandidate {
            project: "proj".to_string(),
            ep_a: ep_a.to_string(),
            ep_b: ep_b.to_string(),
            ts_a: chrono::Utc::now(),
            ts_b: chrono::Utc::now(),
            relation: Relation::ExtendedBy,
            generator: Generator::Relapse,
            topic_key: "symbol:foo".to_string(),
            receipt: PairReceipt {
                receipt_oid: None,
                symbol: "foo".to_string(),
                hashes: vec![],
            },
            aux_oid: None,
            oid_provenance: OidProvenance::CreatedAtFallback,
            event_time: chrono::Utc::now(),
        }
    }

    #[test]
    fn fill_quotes_for_pair_wires_a_real_pair_candidate_end_to_end() {
        let conn = open();
        insert_episode(
            &conn,
            "ep-x",
            "investigate the crash",
            "found the root cause",
        );
        insert_episode(&conn, "ep-y", "patch the crash", "patched the crash");
        let pair = seed_pair_candidate("ep-x", "ep-y");
        let (quote_a, quote_b) = fill_quotes_for_pair(&conn, &pair).unwrap();
        assert!(quote_a.is_verbatim());
        assert!(quote_b.is_verbatim());
    }

    // -----------------------------------------------------------------
    // classify_relapse tiers (pure text logic)
    // -----------------------------------------------------------------

    fn strip_git_env(cmd: &mut Command) {
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(&k);
            }
        }
    }

    fn init_repo(dir: &Path) -> bool {
        std::fs::create_dir_all(dir).unwrap();
        let mut cmd = Command::new("git");
        strip_git_env(&mut cmd);
        cmd.arg("init").arg("-q").arg(dir);
        cmd.status().map(|s| s.success()).unwrap_or(false)
    }

    #[test]
    fn classify_relapse_exact_when_post_matches_pre_byte_for_byte() {
        let pre = "fn foo() {\n    1\n}\n";
        let at_death = "fn foo() {\n    2\n}\n";
        let post = "fn foo() {\n    1\n}\n";
        // repo_root/oids are unused on this tier (checked before the git
        // caller-count fallback), so a nonexistent repo is fine here.
        let class = classify_relapse(
            pre,
            at_death,
            post,
            "/nonexistent",
            "d0",
            "d1",
            "foo",
            "*.rs",
        );
        assert_eq!(class, ClaimClass::ResurrectionExact);
    }

    #[test]
    fn classify_relapse_cleanup_when_only_whitespace_changed_since_death() {
        let pre = "fn foo() {\n    1\n}\n";
        let at_death = "fn foo() {\n    2\n}\n";
        let post = "fn foo() {\n  2\n}\n"; // re-indented, same tokens as at_death
        let class = classify_relapse(
            pre,
            at_death,
            post,
            "/nonexistent",
            "d0",
            "d1",
            "foo",
            "*.rs",
        );
        assert!(matches!(class, ClaimClass::Cleanup { .. }));
        assert!(class.demoted());
    }

    #[test]
    fn classify_relapse_partial_when_line_overlap_with_pre_is_high() {
        let pre = "fn foo() {\n    let a = 1;\n    let b = 2;\n    a + b\n}\n";
        let at_death = "fn foo() {\n    99\n}\n";
        let post = "fn foo() {\n    let a = 1;\n    let b = 2;\n    a + b\n    // extra\n}\n";
        let class = classify_relapse(
            pre,
            at_death,
            post,
            "/nonexistent",
            "d0",
            "d1",
            "foo",
            "*.rs",
        );
        assert!(matches!(class, ClaimClass::ResurrectionPartial { jaccard } if jaccard >= 0.6));
        assert!(!class.demoted());
    }

    #[test]
    fn classify_relapse_migration_when_caller_count_shifts_in_a_real_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return; // git unavailable -- fail-soft skip
        }
        // death_oid state: symbol called once. `git grep -c` counts
        // MATCHING LINES per file (not raw occurrences), so each call sits
        // on its own line to make the count mean what this test needs.
        std::fs::write(repo.join("caller.rs"), "fn user() {\n    foo();\n}\n").unwrap();
        let run = |args: &[&str]| -> bool {
            let mut cmd = Command::new("git");
            strip_git_env(&mut cmd);
            cmd.arg("-C").arg(&repo).args(args);
            cmd.status().map(|s| s.success()).unwrap_or(false)
        };
        assert!(run(&["add", "-A"]));
        assert!(run(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "d0"
        ]));
        let d0 = git_head_oid_for_test(&repo);

        // post_oid state: symbol called on three separate lines -- a real
        // caller-count shift.
        std::fs::write(
            repo.join("caller.rs"),
            "fn user() {\n    foo();\n    foo();\n    foo();\n}\n",
        )
        .unwrap();
        assert!(run(&["add", "-A"]));
        assert!(run(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "d1"
        ]));
        let d1 = git_head_oid_for_test(&repo);

        let pre = "fn foo() {}\n";
        let at_death = "fn foo() {}\n";
        let post = "fn foo(x: i32) {}\n"; // not byte-identical, not a formatting-only change
        let class = classify_relapse(
            pre,
            at_death,
            post,
            &repo.to_string_lossy(),
            &d0,
            &d1,
            "foo",
            "*.rs",
        );
        assert!(matches!(class, ClaimClass::Migration { caller_delta } if caller_delta != 0));
    }

    // -----------------------------------------------------------------
    // Codex review pass 1, finding #6: three concrete predicate fixes.
    // -----------------------------------------------------------------

    #[test]
    fn classify_relapse_whitespace_equal_but_not_byte_identical_is_cleanup_not_resurrection_exact()
    {
        // Re-indented relative to `pre` -- whitespace-equal, but NOT
        // byte-identical. `ResurrectionExact` is documented as
        // byte-identical; this must land as `Cleanup`, never `ResurrectionExact`.
        let pre = "fn foo() {\n    1\n}\n";
        let at_death = "fn foo() {\n    99\n}\n";
        let post = "fn foo() {\n  1\n}\n"; // same tokens/whitespace-squashed as `pre`, different bytes
        let class = classify_relapse(
            pre,
            at_death,
            post,
            "/nonexistent",
            "d0",
            "d1",
            "foo",
            "*.rs",
        );
        assert!(
            matches!(class, ClaimClass::Cleanup { .. }),
            "whitespace-equal-but-not-byte-identical must be Cleanup, got {class:?}"
        );
        assert_ne!(class, ClaimClass::ResurrectionExact);
    }

    #[test]
    fn classify_relapse_reordered_body_does_not_score_a_false_full_resurrection() {
        // `post` is `pre`'s four lines in FULL REVERSE order -- the exact
        // multiset of lines, none of the original sequence. A set/multiset
        // Jaccard scores this 1.0 (false full resurrection); the
        // sequence-aware LCS similarity must score it far below the 0.6
        // partial-resurrection threshold.
        let pre = "fn foo() {\n    a();\n    b();\n    c();\n    d();\n}\n";
        let post = "fn foo() {\n    d();\n    c();\n    b();\n    a();\n}\n";
        let at_death = "fn foo() {\n    completely_different_body();\n}\n";
        let class = classify_relapse(
            pre,
            at_death,
            post,
            "/nonexistent",
            "d0",
            "d1",
            "foo",
            "*.rs",
        );
        assert!(
            !matches!(class, ClaimClass::ResurrectionPartial { .. }),
            "a reordered body must not score as a partial resurrection, got {class:?}"
        );
    }

    #[test]
    fn classify_relapse_a_real_git_failure_on_one_side_never_manufactures_a_migration() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return; // git unavailable -- fail-soft skip
        }
        let run = |args: &[&str]| -> bool {
            let mut cmd = Command::new("git");
            strip_git_env(&mut cmd);
            cmd.arg("-C").arg(&repo).args(args);
            cmd.status().map(|s| s.success()).unwrap_or(false)
        };
        // A real, resolvable commit where `foo` genuinely has callers --
        // the old code's `0` stand-in for a git failure on the OTHER side
        // would diff against this real, nonzero count and manufacture a
        // spurious Migration.
        std::fs::write(repo.join("caller.rs"), "fn user() {\n    foo();\n}\n").unwrap();
        assert!(run(&["add", "-A"]));
        assert!(run(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "d1"
        ]));
        let post_oid = git_head_oid_for_test(&repo);

        // A syntactically-invalid oid: `git grep -c foo <this> -- pathspec`
        // exits >= 2 (bad revision), a REAL failure -- not "zero matches".
        let bogus_death_oid = "not-a-real-oid";

        let pre = "fn foo() {}\n";
        let at_death = "fn foo() {}\n";
        let post = "fn foo(x: i32) {}\n";
        let class = classify_relapse(
            pre,
            at_death,
            post,
            &repo.to_string_lossy(),
            bogus_death_oid,
            &post_oid,
            "foo",
            "*.rs",
        );
        assert_eq!(
            class,
            ClaimClass::Unclear,
            "a real git failure on one side must never manufacture a Migration delta, got {class:?}"
        );
    }

    fn git_head_oid_for_test(repo: &Path) -> String {
        let mut cmd = Command::new("git");
        strip_git_env(&mut cmd);
        cmd.arg("-C").arg(repo).arg("rev-parse").arg("HEAD");
        let out = cmd.output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    // -----------------------------------------------------------------
    // zombie_recurrence
    // -----------------------------------------------------------------

    #[test]
    fn zombie_recurrence_finds_a_post_death_re_proposal() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return;
        }
        let run = |args: &[&str]| -> bool {
            let mut cmd = Command::new("git");
            strip_git_env(&mut cmd);
            cmd.arg("-C").arg(&repo).args(args);
            cmd.status().map(|s| s.success()).unwrap_or(false)
        };
        std::fs::write(repo.join("a.rs"), "// nothing yet\n").unwrap();
        assert!(run(&["add", "-A"]));
        assert!(run(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "d0"
        ]));
        let death_oid = git_head_oid_for_test(&repo);

        std::fs::write(repo.join("a.rs"), "let retry_forever = true;\n").unwrap();
        assert!(run(&["add", "-A"]));
        assert!(run(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "revival"
        ]));
        let head_oid = git_head_oid_for_test(&repo);

        let hits = zombie_recurrence(
            &repo.to_string_lossy(),
            "retry_forever",
            &death_oid,
            &head_oid,
            "*.rs",
            &HashSet::new(),
        );
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn zombie_recurrence_excludes_the_successors_own_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return;
        }
        let run = |args: &[&str]| -> bool {
            let mut cmd = Command::new("git");
            strip_git_env(&mut cmd);
            cmd.arg("-C").arg(&repo).args(args);
            cmd.status().map(|s| s.success()).unwrap_or(false)
        };
        std::fs::write(repo.join("a.rs"), "// nothing yet\n").unwrap();
        assert!(run(&["add", "-A"]));
        assert!(run(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "d0"
        ]));
        let death_oid = git_head_oid_for_test(&repo);

        std::fs::write(repo.join("a.rs"), "let retry_forever = true;\n").unwrap();
        assert!(run(&["add", "-A"]));
        assert!(run(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "successor"
        ]));
        let head_oid = git_head_oid_for_test(&repo);

        let mut exclude = HashSet::new();
        exclude.insert(head_oid.clone());
        let hits = zombie_recurrence(
            &repo.to_string_lossy(),
            "retry_forever",
            &death_oid,
            &head_oid,
            "*.rs",
            &exclude,
        );
        assert!(
            hits.is_empty(),
            "the successor's own commit is not a re-proposal"
        );
    }
}
