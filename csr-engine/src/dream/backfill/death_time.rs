//! A-b: backfill-robust death-time resolution for G-relapse (dream backfill
//! pass 1).
//!
//! # The defect this replaces
//!
//! [`super::pairs::generate_relapse_pairs`] previously resolved a verdict's
//! "event time" (`resolve_event_time`) by trying the committer date of
//! `witness_verdicts.receipt_oid` and falling back to the verdict's own
//! `created_at` (an import/backfill wall-clock timestamp, not evidence of
//! when the underlying code actually died). On a corpus produced by a bulk
//! backfill run, BOTH of those bases can be near-worthless: a large share
//! of verdicts can share one `created_at` day (the backfill's own run time,
//! not the funeral's), and/or `receipt_oid` can be drawn from a tiny set of
//! distinct commits (the backfill's own observed-HEAD-at-run-time, stamped
//! onto every verdict it minted that day) — "git-derived" in name only.
//! Either basis silently orders relapse pairs by backfill-run time instead
//! of by when the code actually changed.
//!
//! This module resolves death time the way the evidence actually supports
//! it, by locating the witness's own `symbol` by NAME at each first-parent
//! commit between its `at_oid` and `HEAD` — never by re-hashing the
//! `witness_ledger`-recorded 0-based `span_start`/`span_end` line
//! coordinates at fixed offsets. The line-coordinate approach this replaces
//! had a specific, confirmed false-positive: a line inserted ABOVE an
//! otherwise byte-identical function shifts every later line number, so
//! re-slicing the ORIGINAL numeric span at a later commit grabs the wrong
//! bytes and reports a death that never happened. [`extract_named_block`]
//! instead re-locates the symbol's definition by searching for it as a
//! whole word on a declaration-shaped line and brace/decl-balancing from
//! there, at EVERY commit independently — content-anchored, immune to
//! unrelated line churn elsewhere in the file. The first commit where that
//! freshly-located block's digest (same [`StampKind`] discipline that
//! produced the stored `stamp`) no longer matches the witness's own
//! at-origin digest is the death commit.
//!
//! # Ancestry gate
//!
//! `at_oid..HEAD` is only a real line of descent when
//! `git merge-base --is-ancestor <at_oid> HEAD` holds ([`merge_base_is_ancestor`]).
//! A rebase, squash, or force-push can leave `at_oid` on a branch HEAD never
//! merged from; in that case the "range" git would compute is a
//! set-difference across two unrelated graphs, not a walk forward in time
//! from the witness. This module refuses to fabricate a death from that —
//! [`try_span_verified_walk`] returns `None`, which degrades resolution to
//! the weaker (and, for the fallback tier, explicitly unorderable) tiers
//! below rather than reporting a confidently-wrong [`Provenance::GitVerifiedWalk`].
//!
//! # Renames
//!
//! When the tracked path stops resolving at a candidate commit, this module
//! checks whether THAT SPECIFIC commit renamed it (`git diff -M
//! --name-status <oid>^ <oid>`, [`detect_rename_at`]) before concluding the
//! symbol died — a plain `git mv` (or a rename `git` itself detects via
//! content similarity) updates the tracked path and the walk continues
//! under the new name. A rename git cannot detect (content rewritten enough
//! that similarity detection misses it) still reads as death — no stronger
//! guarantee is possible without full content-addressed history search,
//! which is out of scope here.
//!
//! # Bounding the walk
//!
//! The historically expensive part of this walk is not the commit
//! enumeration — `git rev-list --first-parent --reverse <at_oid>..<HEAD>`
//! with no pathspec is a cheap pointer-chase, no diffing — it is the
//! PER-COMMIT `git show <oid>:<path>` + name search this module then runs.
//! [`first_parent_oids_capped`] caps the candidate list to
//! [`MAX_WALK_COMMITS`] entries BEFORE any of those per-commit checks run
//! (the previous implementation ran an already-path-filtered `git log`
//! — itself an O(range) diff against one file — and only capped the
//! in-memory `Vec` afterward, which bounded nothing about the subprocess
//! cost). One residual, documented limit: because `-n` combined with
//! `--reverse` returns the N commits NEAREST HEAD rather than nearest
//! `at_oid` (verified empirically; git applies the count limit to its
//! internal newest-first traversal order before reversing for display),
//! the cap here is applied in Rust to the un-capped-at-the-git-level
//! reversed hash list rather than via a `-n` flag — on a first-parent
//! mainline that pointer-chase is cheap even into the hundreds of
//! thousands of commits, so this is a deliberate, documented tradeoff
//! rather than the "expensive scan capped only after the fact" defect it
//! replaces.
//!
//! Only when the walk cannot even be attempted (no `at_oid`, no resolvable
//! repo, span/symbol didn't verify at its own claimed origin, `at_oid` is
//! not an ancestor of `HEAD`, or the symbol is still alive at HEAD — a
//! premature funeral, evidence for resurrection, not relapse) does
//! resolution fall through to progressively weaker, explicitly labeled
//! [`Provenance`] tiers — and the weakest tier,
//! [`Provenance::CreatedAtFallback`], is *never* treated as orderable by
//! [`DeathTime::orderable`], so it can never satisfy a relapse
//! before/after gate.
//!
//! A residual, honestly-documented limit: when the witness carries no
//! `symbol` name at all (nullable in `witness_ledger`), there is nothing to
//! search for by name, so this module falls back to the OLD fixed-line-span
//! re-hashing for that row only — the same false-positive-on-insertion
//! exposure the name-based path exists to close, scoped to the minority of
//! rows that carry no symbol identity to search by.
//!
//! `anchor_reinstated` verdicts are never seen here at all:
//! `witness_verdicts::VerdictKind::is_negative` already excludes them from
//! ever becoming a `SymbolVerdictState::representative`, so they never
//! reach `generate_relapse_pairs`'s per-symbol loop — the "route
//! anchor_reinstated to resurrection, never relapse" invariant A-b calls
//! for is already structurally guaranteed upstream, not something this
//! module has to re-implement.
//!
//! Zero LLM calls anywhere in this module.

use std::path::Path;
use std::process::Command;

use anyhow::Result;
use chrono::{DateTime, Utc};
use codewitness::StampKind;
use rusqlite::{params, Connection, OptionalExtension};

use crate::extraction::repo_root::repo_root_for_file;
use crate::storage::witness_ledger::{witness_by_id, WitnessLedgerRow};
use crate::storage::witness_verdicts::{VerdictKind, WitnessVerdictRow};
use crate::temporal::parse_timestamp;

use super::family::Family;

/// How a [`DeathTime`] was derived, weakest-evidence last. Only the first
/// two are ever [`DeathTime::orderable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// Span-bisection succeeded: a specific first-parent commit was found
    /// where the recorded span stopped re-hashing to the recorded stamp.
    /// The strongest tier — content-proven, not merely OID-adjacent.
    GitVerifiedWalk,
    /// The walk could not be attempted or was inconclusive, but the
    /// verdict's own `receipt_oid` independently resolves to a real commit
    /// AND [`BulkFlags::oid_degenerate`] says this family's `receipt_oid`s
    /// are not a degenerate, backfill-repeated handful. Weaker than a
    /// verified walk (an OID sitting in the *right neighborhood*, not proof
    /// the span itself died there), but still real git evidence.
    GitDerivedPerEvent,
    /// Neither of the above: the verdict's own `created_at` observation
    /// timestamp (a wall-clock/backfill-run artifact). **Never orderable**
    /// — see [`DeathTime::orderable`].
    CreatedAtFallback,
}

/// A resolved death time plus the evidence tier that produced it.
#[derive(Debug, Clone)]
pub struct DeathTime {
    pub time: DateTime<Utc>,
    pub provenance: Provenance,
    /// The commit that actually killed the span ([`Provenance::GitVerifiedWalk`]
    /// only, modulo successor corroboration — see [`try_span_verified_walk`]).
    pub death_oid: Option<String>,
}

impl DeathTime {
    /// The gate contract A-b exists to enforce: a [`Provenance::CreatedAtFallback`]
    /// time is UNORDERABLE and must never satisfy a relapse before/after
    /// gate, no matter how plausible it looks.
    pub fn orderable(&self) -> bool {
        !matches!(self.provenance, Provenance::CreatedAtFallback)
    }

    /// Map onto the pipeline's existing 2-variant OID-provenance vocabulary
    /// (`dream_relations.oid_provenance`'s CHECK constraint has no third
    /// value): both git-evidenced tiers count as "git derived" for that
    /// downstream column; only the fallback tier is "created_at_fallback".
    pub(super) fn oid_provenance(&self) -> super::pairs::OidProvenance {
        match self.provenance {
            Provenance::CreatedAtFallback => super::pairs::OidProvenance::CreatedAtFallback,
            Provenance::GitVerifiedWalk | Provenance::GitDerivedPerEvent => {
                super::pairs::OidProvenance::GitDerived
            }
        }
    }
}

/// Family-level bulk/degeneracy flags from [`detect_bulk`].
#[derive(Debug, Clone, Copy, Default)]
pub struct BulkFlags {
    /// True when this family's CURRENT-STATE negative verdicts carry the
    /// backfill bulk-stamp signature: concentrated on one `created_at` day,
    /// spread across many files, yet their `receipt_oid`s resolve to
    /// commit times spanning a wide historical range — evidence the
    /// backfill swept old history in one run rather than each verdict
    /// carrying its own event's commit. When true,
    /// [`Provenance::GitDerivedPerEvent`] is banned; only a fully
    /// span-verified walk may order.
    pub oid_degenerate: bool,
}

/// F3 fix (Codex review pass 1, finding #4). The PREVIOUS signature —
/// "few distinct `receipt_oid` STRINGS relative to total verdict count" —
/// cannot distinguish a bulk-backfill artifact from a single legitimate
/// refactor commit that genuinely killed many symbols at once: BOTH shapes
/// produce exactly one `receipt_oid` for every affected witness. A
/// 25-symbol/1-commit refactor and a bulk-stamped sweep are numerically
/// identical under that signature, so the old detector flagged the former
/// (a false positive) while also requiring `total >= 20`, which let a
/// smaller-but-real 19-verdict bulk sweep through undetected entirely (a
/// false negative).
///
/// The new signature checks THREE things together, over CURRENT-STATE
/// negative verdicts only (the latest event per witness — a negative event
/// later cancelled by `anchor_reinstated` no longer counts, closing the
/// other half of finding #4):
///
/// 1. **Day concentration**: the dominant `created_at` day accounts for at
///    least [`DAY_SHARE_MIN`] of this family's negative verdicts — the
///    backfill's own run day, not evidence on its own (a real refactor
///    lands, and gets detected, on one day too).
/// 2. **File spread**: those verdicts touch at least [`MIN_DISTINCT_FILES`]
///    distinct files — "many symbols across many files", not one
///    localized change.
/// 3. **Commit-time spread**: `receipt_oid`s that resolve via git span at
///    least [`SPREAD_MIN_DAYS`] of real historical commit time. This is
///    the discriminator a single real commit can never satisfy — one
///    commit is one point in time, never a spread — while a bulk sweep's
///    receipts (drawn from whatever HEAD each backfill pass observed, for
///    witnesses whose UNDERLYING deaths actually happened at many
///    different real times) span real history widely despite landing in
///    the ledger on one day.
///
/// Fewer than 2 resolvable commit times (a single real commit, or no
/// repo/oids resolvable at all) can never satisfy the spread condition —
/// `oid_degenerate` stays `false`: never a false positive on a small,
/// legitimately git-thin, or unresolvable family.
pub fn detect_bulk(conn: &Connection, family: &Family) -> Result<BulkFlags> {
    if family.members.is_empty() {
        return Ok(BulkFlags::default());
    }
    const MIN_TOTAL: i64 = 15;
    const DAY_SHARE_MIN: f64 = 0.5;
    const MIN_DISTINCT_FILES: i64 = 5;
    const SPREAD_MIN_DAYS: i64 = 14;
    /// Repo-resolution + `git show`/committer-date calls are per-oid; cap
    /// how many DISTINCT oids get resolved so a pathological family can't
    /// spend an unbounded number of subprocess calls here.
    const MAX_OIDS_RESOLVED: usize = 512;

    let placeholders: Vec<String> = (1..=family.members.len())
        .map(|i| format!("?{i}"))
        .collect();
    let in_clause = placeholders.join(",");
    // Latest-event-per-witness only (finding #4's other half): a witness
    // whose negative verdict was later cancelled by `anchor_reinstated` no
    // longer contributes to this family's CURRENT bulk signature, matching
    // the join granularity `witness_verdicts::symbol_verdict_state`
    // (relapse generation) already applies.
    let sql = format!(
        "SELECT w.file, substr(v.created_at, 1, 10), v.receipt_oid
         FROM witness_verdicts v
         JOIN witness_ledger w ON w.id = v.witness_id
         WHERE w.project IN ({in_clause})
           AND v.verdict IN ('anchor_obsolete','superseded_by')
           AND v.id = (SELECT MAX(v2.id) FROM witness_verdicts v2 WHERE v2.witness_id = v.witness_id)"
    );
    let params: Vec<&dyn rusqlite::ToSql> = family
        .members
        .iter()
        .map(|m| m as &dyn rusqlite::ToSql)
        .collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<(String, String, Option<String>)> = stmt
        .query_map(params.as_slice(), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let total = rows.len() as i64;
    if total < MIN_TOTAL {
        return Ok(BulkFlags::default());
    }

    let distinct_files = rows
        .iter()
        .map(|(f, _, _)| f.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len() as i64;

    let mut day_counts: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
    for (_, day, _) in &rows {
        *day_counts.entry(day.as_str()).or_insert(0) += 1;
    }
    let dominant_day_count = day_counts.values().copied().max().unwrap_or(0);
    let day_share = dominant_day_count as f64 / total as f64;

    if day_share < DAY_SHARE_MIN || distinct_files < MIN_DISTINCT_FILES {
        // No concentrated-day-across-many-files shape at all: whatever this
        // is, it isn't the bulk-sweep signature.
        return Ok(BulkFlags::default());
    }

    // First recorded file per distinct receipt_oid — repo resolution only
    // needs one real file per oid to find the right repo.
    let mut oid_to_file: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for (file, _, oid) in &rows {
        if let Some(oid) = oid.as_deref() {
            oid_to_file.entry(oid).or_insert(file.as_str());
        }
    }
    let mut times = std::collections::BTreeSet::new();
    for (oid, file) in oid_to_file.iter().take(MAX_OIDS_RESOLVED) {
        if let Some(repo_root) = repo_root_for_file(file) {
            if let Some(t) = git_commit_committer_ts(&repo_root, oid) {
                times.insert(t);
            }
        }
    }
    if times.len() < 2 {
        // A single real commit (or nothing resolvable at all) is never a
        // "spread" — this is exactly the shape a legitimate one-commit
        // refactor that killed many symbols at once produces. Never flag it.
        return Ok(BulkFlags::default());
    }
    let spread_days = (times.iter().next_back().unwrap() - times.iter().next().unwrap()) / 86_400;
    Ok(BulkFlags {
        oid_degenerate: spread_days >= SPREAD_MIN_DAYS,
    })
}

// ---------------------------------------------------------------------
// git shell helpers (small, dependency-free of sibling modules — same
// convention pairs.rs/verify.rs already established)
// ---------------------------------------------------------------------

pub(super) fn git_at(repo_root: &str) -> Command {
    let mut cmd = Command::new("git");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&k);
        }
    }
    cmd.arg("-C").arg(repo_root);
    cmd
}

pub(super) fn git_head_oid(repo_root: &str) -> Option<String> {
    let output = git_at(repo_root)
        .arg("rev-parse")
        .arg("HEAD")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn git_commit_committer_ts(repo_root: &str, oid: &str) -> Option<i64> {
    let output = git_at(repo_root)
        .arg("show")
        .arg("-s")
        .arg("--format=%ct")
        .arg(oid)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .parse::<i64>()
        .ok()
}

/// `git show <oid>:<relpath>` raw bytes. `None` on any failure (path
/// missing at that commit, unresolvable oid, no repo) — never a guess.
pub(super) fn git_show_bytes(repo_root: &str, oid: &str, relpath: &str) -> Option<Vec<u8>> {
    let spec = format!("{oid}:{relpath}");
    let output = git_at(repo_root).arg("show").arg(&spec).output().ok()?;
    if output.status.success() {
        Some(output.stdout)
    } else {
        None
    }
}

/// `git merge-base --is-ancestor <ancestor> <descendant>` — exit 0 means
/// `ancestor` really is reachable from `descendant`. Exit 1 (not an
/// ancestor) and any error (unresolvable OID, no repo) both collapse to
/// `false` via `.success()` — the caller treats "not an ancestor" and
/// "cannot tell" identically: refuse to trust `at_oid..head` as a real line
/// of descent either way, never fabricate a death from a set-difference
/// across rewritten history.
fn merge_base_is_ancestor(repo_root: &str, ancestor_oid: &str, descendant_oid: &str) -> bool {
    git_at(repo_root)
        .arg("merge-base")
        .arg("--is-ancestor")
        .arg(ancestor_oid)
        .arg(descendant_oid)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// First-parent commit oids+committer-times, oldest-first, in
/// `at_oid..head` — deliberately WITHOUT a `-- <path>` pathspec (see the
/// module doc's "Bounding the walk" section: a pathspec turns this into an
/// O(range) diff-against-one-file scan; a bare first-parent `rev-list` is a
/// cheap pointer-chase). Capped to `max` entries in Rust BEFORE the
/// caller's per-commit `git show`+name-search work ever runs against any of
/// them — that per-commit work, not this enumeration, is what
/// [`MAX_WALK_COMMITS`] actually needs to bound.
fn first_parent_oids_capped(
    repo_root: &str,
    at_oid: &str,
    head: &str,
    max: usize,
) -> Vec<(String, i64)> {
    let range = format!("{at_oid}..{head}");
    let Ok(output) = git_at(repo_root)
        .arg("log")
        .arg("--first-parent")
        .arg("--reverse")
        .arg("--format=%H%x09%ct")
        .arg(&range)
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, '\t');
            let oid = parts.next()?.to_string();
            let ts = parts.next()?.trim().parse::<i64>().ok()?;
            Some((oid, ts))
        })
        .take(max)
        .collect()
}

/// When `git show <oid>:<old_relpath>` fails, check whether commit `oid`
/// ITSELF renamed that path (`git diff -M --name-status <oid>^ <oid>`).
/// `Some(new_path)` only on a rename git's own similarity detector
/// recognizes for exactly this commit and exactly this old path — never a
/// guess, and never a multi-commit reconstruction (the walk loop re-tries
/// this at every subsequent commit, so a chain of renames is still followed
/// one hop per commit).
fn detect_rename_at(repo_root: &str, oid: &str, old_relpath: &str) -> Option<String> {
    let output = git_at(repo_root)
        .arg("diff")
        .arg("-M")
        .arg("--name-status")
        .arg(format!("{oid}^"))
        .arg(oid)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut parts = line.split('\t');
        let status = parts.next()?;
        if !status.starts_with('R') {
            continue;
        }
        let old = parts.next()?;
        let new = parts.next()?;
        if old == old_relpath {
            return Some(new.to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------
// Name-anchored symbol block extraction (F1 fix: locate by IDENTITY, not
// by fixed line coordinates that drift when unrelated lines shift above).
// ---------------------------------------------------------------------

/// Declaration-shaped keywords, deliberately excluding `let`/plain
/// assignment: a call site or a local variable happening to share the
/// symbol's name must never be mistaken for its definition.
const DEF_KEYWORDS: [&str; 11] = [
    "fn ",
    "struct ",
    "enum ",
    "trait ",
    "impl ",
    "class ",
    "def ",
    "function ",
    "const ",
    "static ",
    "type ",
];

fn line_has_word(line: &str, word: &str) -> bool {
    line.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|t| t == word)
}

/// First line index containing `symbol` as a whole word, preferring a
/// declaration-shaped line (one of [`DEF_KEYWORDS`]) over a bare mention —
/// falls back to the first bare whole-word occurrence when no
/// declaration-shaped line exists (covers languages/idioms this keyword
/// list doesn't anticipate, still content-anchored rather than a guess at a
/// fixed offset).
fn find_definition_line(lines: &[&str], symbol: &str) -> Option<usize> {
    for (i, line) in lines.iter().enumerate() {
        if line_has_word(line, symbol) && DEF_KEYWORDS.iter().any(|k| line.contains(k)) {
            return Some(i);
        }
    }
    lines.iter().position(|line| line_has_word(line, symbol))
}

/// Extract `symbol`'s definition block from `content` by NAME, not by
/// stored line coordinates: locate the declaration line
/// ([`find_definition_line`]), then brace-balance forward (or stop at a
/// `;`-terminated bodyless declaration) — the same content-anchored
/// heuristic regardless of which commit `content` came from, so a line
/// inserted above this block in a later commit shifts nothing this
/// function depends on. `None` when the symbol cannot be found at all
/// (deleted, or renamed to something this function has no way to know).
fn extract_named_block(content: &[u8], symbol: &str) -> Option<Vec<u8>> {
    let text = String::from_utf8_lossy(content);
    let lines: Vec<&str> = text.split('\n').collect();
    let start = find_definition_line(&lines, symbol)?;
    let mut depth = 0i32;
    let mut opened = false;
    let mut end = start;
    for (i, line) in lines.iter().enumerate().skip(start) {
        for c in line.chars() {
            match c {
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '}' => depth -= 1,
                _ => {}
            }
        }
        end = i;
        if opened && depth <= 0 {
            break;
        }
        if !opened && line.trim_end().ends_with(';') {
            break;
        }
    }
    let mut out = Vec::new();
    for line in &lines[start..=end] {
        out.extend_from_slice(line.as_bytes());
        out.push(b'\n');
    }
    Some(out)
}

/// [`extract_named_block`] at a specific commit, hashed under the same
/// [`StampKind`] discipline the stored `stamp` used — directly comparable
/// to `row.stamp`/an earlier call's digest. `None` when the file/commit is
/// unresolvable OR the symbol cannot be located there at all.
fn name_digest_at(
    repo_root: &str,
    oid: &str,
    relpath: &str,
    symbol: &str,
    kind: StampKind,
) -> Option<String> {
    let bytes = git_show_bytes(repo_root, oid, relpath)?;
    let block = extract_named_block(&bytes, symbol)?;
    Some(kind.compute(&block).as_str().to_string())
}

/// `abs_file`, made relative to `repo_root`, git-path style (forward
/// slashes). Both sides are canonicalized before stripping so a
/// symlink-resolved toplevel (what [`repo_root_for_file`] returns) still
/// matches an unresolved-spelling `abs_file` — falls back to the
/// non-canonical strip when either side no longer exists on disk.
pub(super) fn repo_relative_path(repo_root: &str, abs_file: &str) -> Option<String> {
    let root =
        std::fs::canonicalize(repo_root).unwrap_or_else(|_| Path::new(repo_root).to_path_buf());
    let file =
        std::fs::canonicalize(abs_file).unwrap_or_else(|_| Path::new(abs_file).to_path_buf());
    let rel = file.strip_prefix(&root).ok()?;
    let joined = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

// ---------------------------------------------------------------------
// Stamp re-derivation — mirrors `codewitness::auditor::apply_span_ref`
// exactly (that function is crate-private to `codewitness`, so this is a
// deliberate, small, byte-for-byte mirror rather than a shared dependency;
// `import::backfill`'s own `+1` conversion from 0-based `witness_ledger`
// spans to `codewitness::Anchor`'s 1-based inclusive spans is followed
// here too, so a walk digest and the original stamp are directly
// comparable).
// ---------------------------------------------------------------------

fn stamp_kind_of(stamp: &str) -> StampKind {
    if let Some(rest) = stamp.strip_prefix("b3n") {
        if rest.starts_with(':') {
            return StampKind::Normalized;
        }
    }
    StampKind::Raw
}

fn slice_span(bytes: &[u8], start_1based: u32, end_1based: u32) -> Option<Vec<u8>> {
    if start_1based == 0 || start_1based > end_1based {
        return None;
    }
    let ends_with_newline = bytes.ends_with(b"\n");
    let mut lines: Vec<&[u8]> = if bytes.is_empty() {
        Vec::new()
    } else {
        bytes.split(|&b| b == b'\n').collect()
    };
    if ends_with_newline {
        lines.pop();
    }
    let available = lines.len();
    let start_idx = (start_1based - 1) as usize;
    let end_idx = (end_1based - 1) as usize;
    if start_idx >= available || end_idx >= available {
        return None;
    }
    let mut out = Vec::new();
    for line in &lines[start_idx..=end_idx] {
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    Some(out)
}

fn recompute_stamp(
    kind: StampKind,
    content: &[u8],
    span_start: Option<i64>,
    span_end: Option<i64>,
) -> Option<String> {
    match (span_start, span_end) {
        (Some(s), Some(e)) if s >= 0 && e >= s => {
            let sliced = slice_span(content, (s + 1) as u32, (e + 1) as u32)?;
            Some(kind.compute(&sliced).as_str().to_string())
        }
        _ => Some(kind.compute(content).as_str().to_string()),
    }
}

fn span_matches(
    repo_root: &str,
    oid: &str,
    relpath: &str,
    span_start: Option<i64>,
    span_end: Option<i64>,
    kind: StampKind,
    expected: &str,
) -> bool {
    match git_show_bytes(repo_root, oid, relpath) {
        Some(bytes) => {
            recompute_stamp(kind, &bytes, span_start, span_end).as_deref() == Some(expected)
        }
        None => false,
    }
}

const MAX_WALK_COMMITS: usize = 5000;

/// Tier 1: span-bisection over first-parent history from `row.at_oid`.
/// `None` when the walk cannot be attempted (no repo, no `at_oid`, the span
/// doesn't even verify at its own claimed origin — rewritten/corrupt
/// history) or is inconclusive (still alive at HEAD — a premature funeral,
/// which is resurrection evidence, never a relapse death). On success,
/// applies `superseded_by` successor corroboration: if the successor
/// witness's own `at_oid` commits strictly earlier than the bisected death,
/// the earlier (successor) time wins — the death cannot postdate the
/// replacement that supposedly caused it.
fn try_span_verified_walk(
    conn: &Connection,
    row: &WitnessLedgerRow,
    verdict: &WitnessVerdictRow,
) -> Option<DeathTime> {
    let at_oid = row.at_oid.as_deref()?;
    let repo_root = repo_root_for_file(&row.file)?;
    let relpath = repo_relative_path(&repo_root, &row.file)?;
    let kind = stamp_kind_of(&row.stamp);
    let symbol = row.symbol.as_deref().filter(|s| !s.is_empty());

    // F1: locate by NAME when a symbol identity exists; the origin digest
    // computed this way must independently match `row.stamp` under the
    // SAME kind before anything downstream trusts it (same discipline the
    // old fixed-span check applied to the stored coordinates).
    let name_origin_digest =
        symbol.and_then(|sym| name_digest_at(&repo_root, at_oid, &relpath, sym, kind));
    let origin_verified = match &name_origin_digest {
        Some(d) => d.as_str() == row.stamp,
        None => span_matches(
            &repo_root,
            at_oid,
            &relpath,
            row.span_start,
            row.span_end,
            kind,
            &row.stamp,
        ),
    };
    if !origin_verified {
        // Refuse to trust the walk if the recorded stamp doesn't even
        // verify at its own claimed origin commit (rewritten/corrupt
        // history, or the name-search heuristic couldn't reproduce a
        // fixed-span stamp for this row's shape — either way, no ground
        // truth to walk from).
        return None;
    }

    let head = git_head_oid(&repo_root)?;
    if head == at_oid {
        return None; // nothing to walk
    }
    // F1 ancestry gate: `at_oid..head` is only a real line of descent when
    // `at_oid` is actually reachable from `head`. A rebase/squash/force-push
    // can leave it stranded on an abandoned graph — that "range" is a
    // set-difference across rewritten history, never evidence of a death.
    if !merge_base_is_ancestor(&repo_root, at_oid, &head) {
        return None;
    }

    let name_by_symbol = symbol.zip(name_origin_digest);

    if let Some((sym, origin_digest)) = name_by_symbol {
        if name_digest_at(&repo_root, &head, &relpath, sym, kind).as_deref()
            == Some(origin_digest.as_str())
        {
            // Still alive at HEAD under its own current path: the funeral
            // was premature — resurrection-exact evidence, not a death.
            return None;
        }
        let mut tracked_relpath = relpath.clone();
        for (oid, ts) in first_parent_oids_capped(&repo_root, at_oid, &head, MAX_WALK_COMMITS) {
            match name_digest_at(&repo_root, &oid, &tracked_relpath, sym, kind) {
                Some(d) if d == origin_digest => continue,
                Some(_) => {
                    // Found the symbol by name and its block digest changed:
                    // a content-proven death, immune to unrelated line
                    // shifts elsewhere in the file.
                    return Some(finalize_death(conn, &repo_root, verdict, ts, oid));
                }
                None => {
                    // Symbol not found under the tracked path at this
                    // commit: either it died here, or THIS commit renamed
                    // the file out from under us — check before concluding
                    // death.
                    if let Some(new_path) = detect_rename_at(&repo_root, &oid, &tracked_relpath) {
                        let renamed_unchanged =
                            name_digest_at(&repo_root, &oid, &new_path, sym, kind).as_deref()
                                == Some(origin_digest.as_str());
                        tracked_relpath = new_path;
                        if renamed_unchanged {
                            continue; // renamed, content unchanged: not a death
                        }
                        // renamed AND changed: death, under the new name.
                    }
                    return Some(finalize_death(conn, &repo_root, verdict, ts, oid));
                }
            }
        }
        // Capped walk exhausted with no break found: inconclusive. Refuse
        // to fabricate a walked death rather than guessing which commit
        // did it.
        return None;
    }

    // No symbol identity to search by (nullable in `witness_ledger`):
    // fall back to the legacy fixed-line-span re-hash for this row only —
    // see the module doc's residual-limits note.
    if span_matches(
        &repo_root,
        &head,
        &relpath,
        row.span_start,
        row.span_end,
        kind,
        &row.stamp,
    ) {
        return None;
    }
    for (oid, ts) in first_parent_oids_capped(&repo_root, at_oid, &head, MAX_WALK_COMMITS) {
        if span_matches(
            &repo_root,
            &oid,
            &relpath,
            row.span_start,
            row.span_end,
            kind,
            &row.stamp,
        ) {
            continue;
        }
        return Some(finalize_death(conn, &repo_root, verdict, ts, oid));
    }
    None
}

/// `superseded_by` successor corroboration (shared by both the name-based
/// and legacy fixed-span walk branches): death should coincide with the
/// successor landing; if the successor's own `at_oid` commits strictly
/// earlier than the bisected death, the earlier time wins and the
/// disagreement is a receipt, not a silent fix.
fn finalize_death(
    conn: &Connection,
    repo_root: &str,
    verdict: &WitnessVerdictRow,
    ts: i64,
    oid: String,
) -> DeathTime {
    let (mut death_ts, mut death_oid) = (ts, oid);
    if verdict.verdict == VerdictKind::SupersededBy {
        if let Some(succ_id) = verdict.successor_witness_id {
            if let Ok(Some(succ_row)) = witness_by_id(conn, succ_id) {
                if let Some(succ_oid) = succ_row.at_oid.as_deref() {
                    if let Some(succ_ts) = git_commit_committer_ts(repo_root, succ_oid) {
                        if succ_ts < death_ts {
                            death_ts = succ_ts;
                            death_oid = succ_oid.to_string();
                        }
                    }
                }
            }
        }
    }
    // `DateTime::from_timestamp` only fails on an out-of-range unix
    // timestamp, which a real `%ct` value never produces; fall back to the
    // raw timestamp's own value clamped through `Utc::now()` only in that
    // unreachable-in-practice case rather than panicking.
    let time = DateTime::<Utc>::from_timestamp(death_ts, 0).unwrap_or_else(Utc::now);
    DeathTime {
        time,
        provenance: Provenance::GitVerifiedWalk,
        death_oid: Some(death_oid),
    }
}

fn latest_verdict_created_at(conn: &Connection, witness_id: i64) -> Result<Option<String>> {
    conn.query_row(
        "SELECT created_at FROM witness_verdicts WHERE witness_id = ?1 ORDER BY id DESC LIMIT 1",
        params![witness_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Resolve one negative verdict's death time for G-relapse gating.
/// `ledger_row` is the witness's own `witness_ledger` row when resolvable
/// (`None` degrades straight to tiers 2/3 using `file` for repo
/// resolution). Tries [`try_span_verified_walk`] first; falls to
/// `receipt_oid`'s committer date (banned when `bulk.oid_degenerate`);
/// falls to the verdict's own `created_at` last — and that last tier is
/// never [`DeathTime::orderable`].
pub(super) fn resolve_relapse_death_time(
    conn: &Connection,
    ledger_row: Option<&WitnessLedgerRow>,
    verdict: &WitnessVerdictRow,
    file: &str,
    bulk: &BulkFlags,
) -> Result<DeathTime> {
    if let Some(row) = ledger_row {
        if let Some(dt) = try_span_verified_walk(conn, row, verdict) {
            return Ok(dt);
        }
    }

    if !bulk.oid_degenerate {
        if let Some(oid) = verdict.receipt_oid.as_deref() {
            if let Some(repo_root) = repo_root_for_file(file) {
                if let Some(ts) = git_commit_committer_ts(&repo_root, oid) {
                    if let Some(time) = DateTime::<Utc>::from_timestamp(ts, 0) {
                        return Ok(DeathTime {
                            time,
                            provenance: Provenance::GitDerivedPerEvent,
                            death_oid: Some(oid.to_string()),
                        });
                    }
                }
            }
        }
    }

    let fallback = latest_verdict_created_at(conn, verdict.witness_id)?
        .and_then(|s| parse_timestamp(&s))
        .unwrap_or_else(Utc::now);
    Ok(DeathTime {
        time: fallback,
        provenance: Provenance::CreatedAtFallback,
        death_oid: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::witness_ledger::{insert_witness, WitnessLedgerRow as LedgerRow};
    use crate::storage::witness_verdicts::{insert_verdict_if_changed, WitnessVerdictRow};

    fn open() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::storage::migrations::run(&conn).unwrap();
        conn
    }

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

    fn commit_all(dir: &Path) -> Option<String> {
        let run = |args: &[&str]| -> bool {
            let mut cmd = Command::new("git");
            strip_git_env(&mut cmd);
            cmd.arg("-C").arg(dir).args(args);
            cmd.status().map(|s| s.success()).unwrap_or(false)
        };
        if !run(&["add", "-A"]) {
            return None;
        }
        if !run(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "seed",
        ]) {
            return None;
        }
        git_head_oid(&dir.to_string_lossy())
    }

    fn seed_witness(
        conn: &Connection,
        project: &str,
        file: &Path,
        stamp: &str,
        at_oid: &str,
    ) -> i64 {
        insert_witness(
            conn,
            &LedgerRow {
                id: 0,
                project: project.to_string(),
                file: file.to_string_lossy().to_string(),
                symbol: Some("foo".to_string()),
                span_start: None,
                span_end: None,
                stamp: stamp.to_string(),
                tier: "committed".to_string(),
                at_oid: Some(at_oid.to_string()),
                source_kind: "backfill".to_string(),
                source_id: None,
            },
        )
        .unwrap();
        conn.query_row(
            "SELECT id FROM witness_ledger WHERE project = ?1 AND file = ?2 AND stamp = ?3",
            params![project, file.to_string_lossy().to_string(), stamp],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn mark(conn: &Connection, witness_id: i64, receipt_oid: Option<&str>, observed_head: &str) {
        insert_verdict_if_changed(
            conn,
            &WitnessVerdictRow {
                witness_id,
                verdict: VerdictKind::AnchorObsolete,
                successor_witness_id: None,
                receipt_oid: receipt_oid.map(str::to_string),
                observed_head_oid: observed_head.to_string(),
            },
        )
        .unwrap();
    }

    // -----------------------------------------------------------------
    // detect_bulk (F3 fix, Codex review pass 1, finding #4): the signature
    // is now day-concentration + file-spread + REAL commit-time spread —
    // not "few distinct receipt_oid strings", which could never tell a
    // bulk sweep apart from one legitimate multi-symbol refactor commit.
    // -----------------------------------------------------------------

    fn commit_all_with_date(dir: &Path, unix_ts: i64) -> Option<String> {
        let date = format!("{unix_ts} +0000");
        let mut add = Command::new("git");
        strip_git_env(&mut add);
        add.arg("-C").arg(dir).arg("add").arg("-A");
        if !add.status().map(|s| s.success()).unwrap_or(false) {
            return None;
        }
        let mut commit = Command::new("git");
        strip_git_env(&mut commit);
        commit
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .arg("-C")
            .arg(dir)
            .arg("-c")
            .arg("user.email=t@example.com")
            .arg("-c")
            .arg("user.name=t")
            .arg("commit")
            .arg("-q")
            .arg("-m")
            .arg("seed");
        if !commit.status().map(|s| s.success()).unwrap_or(false) {
            return None;
        }
        git_head_oid(&dir.to_string_lossy())
    }

    #[test]
    fn detect_bulk_never_flags_a_legitimate_one_commit_refactor_across_many_files() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return; // git unavailable -- fail-soft skip
        }
        // ONE real commit touching 6 files -- a genuine multi-symbol
        // refactor. This is exactly the shape the OLD "few distinct
        // receipt_oid strings" signature misclassified as bulk (finding
        // #4's false positive).
        for i in 0..6 {
            std::fs::write(repo.join(format!("f{i}.rs")), format!("fn f{i}() {{}}\n")).unwrap();
        }
        let Some(oid) = commit_all(&repo) else {
            return;
        };

        let conn = open();
        let family = Family::single("proj");
        // 25 witnesses (well above MIN_TOTAL), all sharing this ONE real
        // receipt_oid, spread across the 6 real files.
        for i in 0..25 {
            let file = repo.join(format!("f{}.rs", i % 6));
            let wid = seed_witness(&conn, "proj", &file, &format!("b3:s{i}"), "deadbeef");
            mark(&conn, wid, Some(&oid), "headoid");
        }
        let flags = detect_bulk(&conn, &family).unwrap();
        assert!(
            !flags.oid_degenerate,
            "a single real commit's own receipt_oid is one point in time, never a spread"
        );
    }

    #[test]
    fn detect_bulk_flags_a_real_sweep_whose_receipt_oids_span_wide_historical_time() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return;
        }
        let now = chrono::Utc::now().timestamp();
        // Three real commits, ~200 days apart each -- the "backfill swept
        // old history in one run" shape: many DIFFERENT real historical
        // commits, all stamped into the ledger on the SAME day (today, as
        // every test insert naturally is).
        std::fs::write(repo.join("f0.rs"), "fn f0() {}\n").unwrap();
        std::fs::write(repo.join("f1.rs"), "fn f1() {}\n").unwrap();
        let Some(oid_old) = commit_all_with_date(&repo, now - 400 * 86_400) else {
            return;
        };
        std::fs::write(repo.join("f2.rs"), "fn f2() {}\n").unwrap();
        std::fs::write(repo.join("f3.rs"), "fn f3() {}\n").unwrap();
        let Some(oid_mid) = commit_all_with_date(&repo, now - 200 * 86_400) else {
            return;
        };
        std::fs::write(repo.join("f4.rs"), "fn f4() {}\n").unwrap();
        std::fs::write(repo.join("f5.rs"), "fn f5() {}\n").unwrap();
        let Some(oid_new) = commit_all_with_date(&repo, now - 40 * 86_400) else {
            return;
        };

        let conn = open();
        let family = Family::single("proj");
        // 19 witnesses (below the OLD MIN_TOTAL of 20 -- finding #4's other
        // false negative), spread across the 6 real files and the 3 real,
        // widely-time-separated receipt_oids.
        let oids = [oid_old, oid_mid, oid_new];
        for i in 0..19 {
            let file = repo.join(format!("f{}.rs", i % 6));
            let wid = seed_witness(&conn, "proj", &file, &format!("b3:s{i}"), "deadbeef");
            mark(&conn, wid, Some(oids[i % 3].as_str()), "headoid");
        }
        let flags = detect_bulk(&conn, &family).unwrap();
        assert!(
            flags.oid_degenerate,
            "verdicts stamped the same day but backed by receipt_oids spanning ~360 real days must flag as bulk"
        );
    }

    #[test]
    fn detect_bulk_excludes_a_witness_later_reinstated_from_the_current_signature() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return;
        }
        let now = chrono::Utc::now().timestamp();
        std::fs::write(repo.join("f0.rs"), "fn f0() {}\n").unwrap();
        std::fs::write(repo.join("f1.rs"), "fn f1() {}\n").unwrap();
        let Some(oid_old) = commit_all_with_date(&repo, now - 400 * 86_400) else {
            return;
        };
        std::fs::write(repo.join("f2.rs"), "fn f2() {}\n").unwrap();
        std::fs::write(repo.join("f3.rs"), "fn f3() {}\n").unwrap();
        let Some(oid_mid) = commit_all_with_date(&repo, now - 200 * 86_400) else {
            return;
        };
        std::fs::write(repo.join("f4.rs"), "fn f4() {}\n").unwrap();
        std::fs::write(repo.join("f5.rs"), "fn f5() {}\n").unwrap();
        let Some(oid_new) = commit_all_with_date(&repo, now - 40 * 86_400) else {
            return;
        };

        let conn = open();
        let family = Family::single("proj");
        let oids = [oid_old, oid_mid, oid_new];
        let mut wids = Vec::new();
        for i in 0..19 {
            let file = repo.join(format!("f{}.rs", i % 6));
            let wid = seed_witness(&conn, "proj", &file, &format!("b3:s{i}"), "deadbeef");
            mark(&conn, wid, Some(oids[i % 3].as_str()), "headoid");
            wids.push(wid);
        }
        // Confirm the un-reinstated corpus is flagged (sanity baseline)...
        assert!(detect_bulk(&conn, &family).unwrap().oid_degenerate);

        // ...then reinstate ALL of them: the CURRENT state for every
        // witness is no longer negative, so this family's bulk signature
        // must disappear along with it (finding #4: the old query counted
        // every historical negative row even when later cancelled).
        for wid in wids {
            insert_verdict_if_changed(
                &conn,
                &WitnessVerdictRow {
                    witness_id: wid,
                    verdict: VerdictKind::AnchorReinstated,
                    successor_witness_id: None,
                    receipt_oid: None,
                    observed_head_oid: "headoid2".to_string(),
                },
            )
            .unwrap();
        }
        let flags = detect_bulk(&conn, &family).unwrap();
        assert!(
            !flags.oid_degenerate,
            "a witness whose latest event is anchor_reinstated must not count toward the bulk signature"
        );
    }

    #[test]
    fn detect_bulk_never_flags_a_small_family() {
        let conn = open();
        let family = Family::single("proj");
        for _ in 0..3 {
            let wid = seed_witness(
                &conn,
                "proj",
                Path::new("/nowhere/f.rs"),
                "b3:s",
                "deadbeef",
            );
            mark(&conn, wid, Some("sharedoid"), "headoid");
        }
        let flags = detect_bulk(&conn, &family).unwrap();
        assert!(
            !flags.oid_degenerate,
            "too few verdicts to call bulk either way"
        );
    }

    // -----------------------------------------------------------------
    // resolve_relapse_death_time: the A-b gate contract
    // -----------------------------------------------------------------

    #[test]
    fn bulk_stamped_family_with_no_resolvable_repo_yields_unorderable_created_at_fallback() {
        let conn = open();
        // No real repo anywhere -- span walk and per-event OID resolution
        // both fail regardless of the bulk flag; this is exactly the
        // "checkout no longer on this machine" shape of the live corpus's
        // degenerate-receipt-oid families.
        let wid = seed_witness(
            &conn,
            "proj",
            Path::new("/nowhere/on/disk/f.rs"),
            "b3:s",
            "deadbeef",
        );
        mark(&conn, wid, Some("sharedoid"), "headoid");
        let row = witness_by_id(&conn, wid).unwrap().unwrap();
        let verdict = WitnessVerdictRow {
            witness_id: wid,
            verdict: VerdictKind::AnchorObsolete,
            successor_witness_id: None,
            receipt_oid: Some("sharedoid".to_string()),
            observed_head_oid: "headoid".to_string(),
        };
        let bulk = BulkFlags {
            oid_degenerate: true,
        };
        let death =
            resolve_relapse_death_time(&conn, Some(&row), &verdict, &row.file, &bulk).unwrap();
        assert_eq!(death.provenance, Provenance::CreatedAtFallback);
        assert!(
            !death.orderable(),
            "CreatedAtFallback must never satisfy the relapse gate"
        );
    }

    #[test]
    fn git_verified_death_walk_orders_correctly() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return; // git unavailable in this environment -- fail-soft skip
        }
        let file = repo.join("a.rs");
        std::fs::write(&file, "fn foo() {\n    1\n}\n").unwrap();
        let Some(c0) = commit_all(&repo) else {
            return;
        };
        let content_at_c0 = std::fs::read(&file).unwrap();
        let stamp = StampKind::Raw.compute(&content_at_c0).as_str().to_string();

        // A later first-parent commit that changes the file -- this is the
        // death commit the walk must find.
        std::fs::write(&file, "fn foo() {\n    2\n}\n").unwrap();
        let Some(c1) = commit_all(&repo) else {
            return;
        };

        let conn = open();
        let wid = seed_witness(&conn, "proj", &file, &stamp, &c0);
        mark(&conn, wid, None, &c1);
        let row = witness_by_id(&conn, wid).unwrap().unwrap();
        let verdict = WitnessVerdictRow {
            witness_id: wid,
            verdict: VerdictKind::AnchorObsolete,
            successor_witness_id: None,
            receipt_oid: None,
            observed_head_oid: c1.clone(),
        };
        let bulk = BulkFlags::default();
        let death =
            resolve_relapse_death_time(&conn, Some(&row), &verdict, &row.file, &bulk).unwrap();
        assert_eq!(death.provenance, Provenance::GitVerifiedWalk);
        assert!(death.orderable());
        assert_eq!(death.death_oid.as_deref(), Some(c1.as_str()));
    }

    #[test]
    fn span_still_alive_at_head_is_not_a_git_verified_death() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return;
        }
        let file = repo.join("a.rs");
        std::fs::write(&file, "fn foo() {\n    1\n}\n").unwrap();
        let Some(c0) = commit_all(&repo) else {
            return;
        };
        let content_at_c0 = std::fs::read(&file).unwrap();
        let stamp = StampKind::Raw.compute(&content_at_c0).as_str().to_string();
        // A later commit that touches an UNRELATED file -- foo's span never
        // actually changes, so it is still alive at HEAD.
        std::fs::write(repo.join("other.rs"), "fn bar() {}\n").unwrap();
        let Some(_c1) = commit_all(&repo) else {
            return;
        };

        let conn = open();
        let wid = seed_witness(&conn, "proj", &file, &stamp, &c0);
        mark(&conn, wid, None, "headoid");
        let row = witness_by_id(&conn, wid).unwrap().unwrap();
        let verdict = WitnessVerdictRow {
            witness_id: wid,
            verdict: VerdictKind::AnchorObsolete,
            successor_witness_id: None,
            receipt_oid: None,
            observed_head_oid: "headoid".to_string(),
        };
        let bulk = BulkFlags::default();
        let death =
            resolve_relapse_death_time(&conn, Some(&row), &verdict, &row.file, &bulk).unwrap();
        // Falls through to CreatedAtFallback: never a fabricated walked death.
        assert_eq!(death.provenance, Provenance::CreatedAtFallback);
    }

    // -----------------------------------------------------------------
    // F1: name-anchored location must survive a line inserted ABOVE a
    // byte-identical symbol, and still catch a real death that follows.
    // Real spans are set (NOT None) — the exact shape the previous test
    // suite dodged (Codex review pass 1, finding #1).
    // -----------------------------------------------------------------

    fn write_and_commit(repo: &Path, file: &Path, content: &str) -> Option<String> {
        std::fs::write(file, content).unwrap();
        commit_all(repo)
    }

    #[test]
    fn name_based_walk_survives_a_line_inserted_above_the_symbol_then_finds_the_real_death() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return; // git unavailable in this environment -- fail-soft skip
        }
        let file = repo.join("a.rs");

        let Some(c0) = write_and_commit(&repo, &file, "fn foo() {\n    1\n}\n") else {
            return;
        };
        let stamp = StampKind::Raw
            .compute(b"fn foo() {\n    1\n}\n")
            .as_str()
            .to_string();

        // c1: insert TWO new lines ABOVE foo. foo's own body is
        // byte-identical, but its line numbers have shifted -- the exact
        // shape that made the fixed-line-span re-hash this replaces report
        // a FALSE death (re-slicing the ORIGINAL 0-2 span at this commit
        // would grab "// unrelated\nfn bar() {}" instead of foo's body).
        let Some(_c1) = write_and_commit(
            &repo,
            &file,
            "// unrelated\nfn bar() {}\n\nfn foo() {\n    1\n}\n",
        ) else {
            return;
        };

        // c2: NOW foo's body actually changes -- this is the real death.
        let Some(c2) = write_and_commit(
            &repo,
            &file,
            "// unrelated\nfn bar() {}\n\nfn foo() {\n    2\n}\n",
        ) else {
            return;
        };

        let conn = open();
        // Real spans, matching foo's ORIGINAL 0-based line coordinates at
        // c0 (lines 0..=2) -- exactly the stale coordinates a fixed-span
        // re-hash would keep re-slicing at every later commit.
        insert_witness(
            &conn,
            &LedgerRow {
                id: 0,
                project: "proj".to_string(),
                file: file.to_string_lossy().to_string(),
                symbol: Some("foo".to_string()),
                span_start: Some(0),
                span_end: Some(2),
                stamp: stamp.clone(),
                tier: "committed".to_string(),
                at_oid: Some(c0.clone()),
                source_kind: "backfill".to_string(),
                source_id: None,
            },
        )
        .unwrap();
        let wid: i64 = conn
            .query_row(
                "SELECT id FROM witness_ledger WHERE project = 'proj' AND stamp = ?1",
                params![stamp],
                |r| r.get(0),
            )
            .unwrap();
        mark(&conn, wid, None, &c2);
        let row = witness_by_id(&conn, wid).unwrap().unwrap();
        let verdict = WitnessVerdictRow {
            witness_id: wid,
            verdict: VerdictKind::AnchorObsolete,
            successor_witness_id: None,
            receipt_oid: None,
            observed_head_oid: c2.clone(),
        };
        let bulk = BulkFlags::default();
        let death =
            resolve_relapse_death_time(&conn, Some(&row), &verdict, &row.file, &bulk).unwrap();
        assert_eq!(
            death.provenance,
            Provenance::GitVerifiedWalk,
            "must find the real death by name, not report a false one at c1"
        );
        assert_eq!(
            death.death_oid.as_deref(),
            Some(c2.as_str()),
            "the line inserted above foo at c1 must NOT read as its death"
        );
        assert!(death.orderable());
    }

    // -----------------------------------------------------------------
    // F1: ancestry gate — `at_oid` stranded on a graph HEAD never merged
    // from must never order a relapse.
    // -----------------------------------------------------------------

    #[test]
    fn ancestry_gate_refuses_to_order_across_rewritten_history() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return;
        }
        let file = repo.join("a.rs");
        let Some(c0) = write_and_commit(&repo, &file, "fn foo() {\n    1\n}\n") else {
            return;
        };
        let stamp = StampKind::Raw
            .compute(b"fn foo() {\n    1\n}\n")
            .as_str()
            .to_string();

        // Rewrite history: an orphan branch sharing NO ancestry with c0
        // becomes the new HEAD -- the shape a rebase/squash/force-push
        // leaves behind. `c0..head` is a set-difference across two
        // unrelated graphs, never a real walk forward from the witness.
        let strip = |cmd: &mut Command| {
            for (k, _) in std::env::vars_os() {
                if k.to_string_lossy().starts_with("GIT_") {
                    cmd.env_remove(&k);
                }
            }
        };
        let mut orphan = Command::new("git");
        strip(&mut orphan);
        orphan
            .arg("-C")
            .arg(&repo)
            .arg("checkout")
            .arg("-q")
            .arg("--orphan")
            .arg("rewritten");
        assert!(orphan.status().unwrap().success());
        std::fs::write(&file, "fn foo() {\n    1\n}\n").unwrap();
        let mut add = Command::new("git");
        strip(&mut add);
        add.arg("-C").arg(&repo).arg("add").arg("-A");
        assert!(add.status().unwrap().success());
        let mut commit = Command::new("git");
        strip(&mut commit);
        commit
            .arg("-C")
            .arg(&repo)
            .arg("-c")
            .arg("user.email=t@example.com")
            .arg("-c")
            .arg("user.name=t")
            .arg("commit")
            .arg("-q")
            .arg("-m")
            .arg("rewritten root");
        assert!(commit.status().unwrap().success());
        let head = git_head_oid(&repo.to_string_lossy()).unwrap();
        assert_ne!(head, c0, "the orphan branch must be a distinct commit");
        assert!(
            !merge_base_is_ancestor(&repo.to_string_lossy(), &c0, &head),
            "the orphan branch must share no ancestry with c0"
        );

        let conn = open();
        let wid = seed_witness(&conn, "proj", &file, &stamp, &c0);
        mark(&conn, wid, None, &head);
        let row = witness_by_id(&conn, wid).unwrap().unwrap();
        let verdict = WitnessVerdictRow {
            witness_id: wid,
            verdict: VerdictKind::AnchorObsolete,
            successor_witness_id: None,
            receipt_oid: None,
            observed_head_oid: head,
        };
        let bulk = BulkFlags::default();
        let death =
            resolve_relapse_death_time(&conn, Some(&row), &verdict, &row.file, &bulk).unwrap();
        assert_ne!(
            death.provenance,
            Provenance::GitVerifiedWalk,
            "must never fabricate a walked death across rewritten/unrelated history"
        );
        assert!(
            !death.orderable(),
            "with no receipt_oid, the only remaining tier is the unorderable fallback"
        );
    }
}
