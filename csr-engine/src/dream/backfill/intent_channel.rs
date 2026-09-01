//! A-e (dream backfill pass 2): `history.jsonl` prompts as a no-death-event
//! evidence channel for episode-starved families.
//!
//! # Why this exists
//!
//! [`super::pairs`]'s generators (ledger/era/relapse) all require a
//! `witness_verdicts` row — a *negative* verdict on some symbol — before
//! they can propose anything. A family whose sessions never populated
//! `episode_index` (or whose code was never git-witnessed at all) has no
//! such verdicts and is invisible to every one of them, no matter how much
//! real work happened there. `anukriti-website` is exactly this shape on
//! the live corpus: 914 `history.jsonl` prompts, effectively zero episode
//! coverage.
//!
//! This module mines a DIFFERENT evidence channel entirely: the raw prompt
//! text a person actually typed, in `~/.claude/history.jsonl`. A prompt
//! that named a concrete approach (a path, an identifier, a backticked
//! span) and was never followed by matching git activity, and was never
//! mentioned again, is evidence the request was silently dropped.
//!
//! INVARIANT: an abandonment candidate fires only when ALL FOUR legs hold,
//! each receipt-carrying:
//!   1. INTENT — prompt P verbatim-names approach X at time T for family F.
//!   2. GIT-NEGATIVE (pass 3 widening — see below) — across ALL branches
//!      and ALL history (not just the pinned HEAD, not just after T): no
//!      commit anywhere ever introduced X's identifier via pickaxe, and no
//!      ever-delivered file in the repo's whole history normalizes to X's
//!      own basename.
//!   3. NON-RECURRENCE — no later prompt in F re-mentions X.
//!   4. SUBAGENT-NEGATIVE (pass 3, free from build 1's
//!      [`super::subagent_citation`]) — no subagent transcript belonging to
//!      a family session AT OR AFTER T authored X (a real edit, not a mere
//!      mention) in a matching file. This catches delivery git itself can't
//!      see (e.g. work done in a worktree/branch never committed here).
//!
//! The SATISFIED-prompt guard is deliberately one-sided: ANY of legs 2 or 4
//! firing marks the prompt SATISFIED and kills the candidate outright. A
//! false "abandoned" is worse than a missed dream, so both are checked to
//! fail closed — see "Pass 3: guard hardening" below for the exact git
//! commands and the honestly-documented residual this still cannot see.
//!
//! Zero LLM calls anywhere in this module.
//!
//! # Pass 3 (this build): guard hardening + `dreams_v1` persistence
//!
//! Two changes over pass 2:
//!
//! - **Guard hardening.** The git-negative leg was previously scoped to a
//!   single ref (the pinned HEAD) and `--since=T` — a false "abandoned" was
//!   possible whenever the identifier landed on a different branch, was
//!   introduced before T then removed, or shipped under a renamed/
//!   different-extension file whose literal identifier the old per-target
//!   pickaxe never tried. Three widenings fix this:
//!     - [`pickaxe_leg_all_history`]: `git log --all -S<ident>
//!       --format=%H %ct`, no `--since` — ANY commit, on ANY branch, at ANY
//!       time, that ever introduced/removed the identifier marks
//!       SATISFIED. The pinned HEAD is now used ONLY as the horizon
//!       receipt in the rendered claim, never to scope the search.
//!     - [`delivered_basenames`]: ONE `git log --all --name-only --format=`
//!       call per family (never per-candidate — see the function doc),
//!       building a normalized (lowercased, known-extension-stripped)
//!       basename set of every file ever committed on any branch. A
//!       target's own normalized basename ([`target_basename_for_scan`])
//!       hitting this set marks SATISFIED even when the file's CONTENT
//!       never literally contained the target string (an empty scaffold
//!       file, a rename across extensions).
//!     - The subagent leg (invariant leg 4 above), reusing build 1's
//!       transcript-scanning machinery for a channel git can't see at all.
//!
//!   Honest residual (undetectable by any of the above, still): a total
//!   rename to a name sharing NO token with the original AND never
//!   mentioned by name in a family subagent transcript — e.g. `widget.rs`
//!   silently reborn as `sidebar-panel.tsx` with no commit message, PR, or
//!   subagent transcript ever using the word "widget" again. No receipt
//!   channel this codebase has can see that; this module does not claim
//!   to.
//! - **Persistence.** [`generate_abandonment_candidates`] now runs as a
//!   real pipeline stage (`csr-engine dream backfill`, after Stage 2-3 —
//!   see [`super::cli`]); [`super::compose::persist_abandonment_candidates`]
//!   drains its surviving candidates into `dreams_v1` under the
//!   pre-existing `category = 'unfinished'` value (reused, not a new CHECK
//!   value — see that function's own doc for why), carrying a
//!   mechanically-audited [`super::subagent_citation::DreamCitationEvidence`]
//!   on every row. `bar_clause_met` truthfully reflects whether ANY family
//!   subagent transcript, UNRESTRICTED by time, authored the target — see
//!   [`build_candidate_evidence`]'s doc for why that is intentionally a
//!   BROADER population than the guard's own leg 4, which only blocks
//!   firing on POST-prompt evidence.
//!
//! # Reconciliation against the real schema (pass 2)
//!
//! The design source this module ports (`ox_turn3.md`) was written against
//! an idealized schema that does not exist in this codebase:
//!
//! - There is no `family_map` table, no `v_episode_family` /
//!   `v_ledger_family` / `v_verdict_family` SQL views, and no
//!   `family::family_for_path` function. Family membership is computed
//!   in-memory by [`super::family::compute_families`] (A-c, repo-identity
//!   union-find over `episode_index`/`witness_ledger` project keys); this
//!   module resolves a raw filesystem path (history.jsonl's `project`
//!   field, always a real cwd, never a DB key) to a family by running the
//!   SAME repo-identity resolution ([`super::family::resolve_repo_identity`])
//!   against that path directly and matching its toplevel against each
//!   family's own resolved toplevel — see [`build_family_repo_maps`].
//! - `episode_index` has no `intent`/`todos`/`family` columns. The
//!   episode-dedup text index is built from the columns that actually
//!   exist (`request`, `completed`, `next_steps`, `blockers`), joined
//!   per-family via an `IN (...)` clause over `Family::members` — the same
//!   pattern [`super::death_time::detect_bulk`] already uses.
//! - `history.jsonl`'s `timestamp` field is `i64` milliseconds on every
//!   row measured on the live corpus (13-digit values), but is normalized
//!   defensively in case an older row is ever recorded in seconds — see
//!   [`normalize_ts_secs`].
//! - There is no shared `gitx` module; small per-file git shell helpers,
//!   duplicated rather than shared, are this codebase's own established
//!   convention (see `family.rs`'s module doc) — this module follows it.
//! - `AbandonmentCandidate` is NOT written to `dream_relations` by this
//!   module. `dream_relations` models a paired `(ep_a, ep_b)` supersession
//!   relation; an abandonment claim has no `ep_b` at all (nothing
//!   superseded it — that is the entire claim), so it does not fit that
//!   table's shape regardless of the `generator` CHECK's allowed values.
//!   Widening `dream_relations.generator` to add `'abandonment'`
//!   speculatively, before any code exists that would insert such a row,
//!   would repeat the exact anti-pattern `dreams_v1`'s own migration
//!   comment warns against avoiding elsewhere in this file: schema churn
//!   with no consumer. This pass therefore leaves the CHECK constraint
//!   untouched; a future Stage 6 extension that decides how an
//!   abandonment claim is actually persisted (a dedicated table, or a
//!   sentinel `ep_b`) makes that schema decision together with the insert
//!   code that needs it. (Pass 3 update: that future extension is THIS
//!   pass — see the module doc's "Pass 3" section above. It persists
//!   directly to `dreams_v1`, never to `dream_relations`; the bullet above
//!   about `dream_relations` itself remains accurate and untouched.)

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::extraction::repo_root::repo_root_for_file;

use super::family;
use super::subagent_citation::{self, DreamCitationEvidence};

/// Pre-registered: younger prompts are unjudgeable (git activity may
/// simply not have caught up yet).
pub const MIN_AGE_DAYS: i64 = 14;
const MAX_TARGETS_PER_PROMPT: usize = 8;
const MAX_CANDIDATES_PER_FAMILY: usize = 50;
/// Default per-family ceiling on git subprocess invocations this stage will
/// spend on the widened all-history pickaxe leg (one per checked target,
/// budgeted separately from [`MAX_CANDIDATES_PER_FAMILY`], which caps FIRED
/// candidates rather than attempts). `pub` so `cli::handle_backfill` can
/// reuse it as the real pipeline's default.
pub const DEFAULT_GIT_BUDGET_PER_FAMILY: usize = 300;
/// A later prompt "recurs" a target when it mentions at least half of the
/// target phrase's content tokens.
const RECURRENCE_CONTAINMENT: f64 = 0.5;
/// A prompt is "already covered by an episode" when at least 80% of its
/// content tokens are contained in some episode's own intent text.
const EPISODE_DUP_CONTAINMENT: f64 = 0.8;

const STOP: &[&str] = &[
    "the", "and", "for", "with", "this", "that", "then", "when", "from", "have", "has", "had",
    "should", "would", "could", "need", "needs", "want", "make", "made", "work", "works", "code",
    "file", "files", "test", "tests", "please", "about", "into", "your", "will", "can", "are",
    "was", "were", "been", "being", "also", "some", "more", "all", "any", "new", "old", "get",
    "set", "run", "use", "used", "using", "add", "added", "update", "fix", "fixed", "error",
    "issue", "change", "check", "look", "see", "help", "read", "write", "like", "just", "now",
    "there", "their", "them", "they", "what", "which", "while", "after", "before", "because",
];

fn normalize(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub(crate) fn content_tokens(s: &str) -> HashSet<String> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| w.len() > 3 && !STOP.contains(&w.to_lowercase().as_str()))
        .map(|w| w.to_lowercase())
        .collect()
}

pub(crate) fn containment(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() {
        return 0.0;
    }
    a.intersection(b).count() as f64 / a.len() as f64
}

/// `history.jsonl` is 13-digit millisecond epoch on every row measured on
/// the live corpus; this defends against a hypothetical second-precision
/// row (a threshold well past any real millisecond timestamp's magnitude
/// when misread as seconds, and well short of any second timestamp's
/// magnitude when correctly read).
fn normalize_ts_secs(raw: i64) -> i64 {
    const MS_THRESHOLD: i64 = 10_000_000_000; // ~year 2286 in seconds
    if raw.abs() > MS_THRESHOLD {
        raw / 1000
    } else {
        raw
    }
}

fn iso_date(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| format!("ts:{ts}"))
}

// ---------------------------------------------------------------------
// family attribution: raw filesystem path -> family name
// ---------------------------------------------------------------------

/// Probe `dir`'s own git-repo toplevel by asking [`repo_root_for_file`]
/// about a (nonexistent) file inside it — that function only ever reads
/// `file.parent()`, so this recovers exactly `dir`'s toplevel without
/// requiring a real file to exist there. `None` when `dir` isn't inside a
/// resolvable git work tree (checkout gone, not a repo, `git` missing).
fn toplevel_of_dir(dir: &str) -> Option<String> {
    if dir.is_empty() {
        return None;
    }
    let probe = format!("{}/__csr_intent_channel_probe__", dir.trim_end_matches('/'));
    repo_root_for_file(&probe)
}

/// One family's own resolved repo-identity toplevel, found by re-running
/// [`family::resolve_repo_identity`] against that family's own member
/// keys (the same resolution [`family::compute_families`] already did
/// once to GROUP those keys — here it is looked up per-family instead, so
/// a raw filesystem path can be matched against it).
fn repo_toplevel_for_family(conn: &Connection, fam: &family::Family) -> Result<Option<PathBuf>> {
    for member in &fam.members {
        let files =
            family::candidate_files_for_project(conn, member, family::CANDIDATE_FILE_SAMPLE_LIMIT)?;
        let identity = family::resolve_repo_identity(&files);
        if let Some(top) = identity.toplevel {
            return Ok(Some(PathBuf::from(top)));
        }
    }
    Ok(None)
}

/// `toplevel -> family name` (for attributing a raw path) and `family name
/// -> toplevel` (for running git against that family's repo), built once
/// per backfill run rather than per prompt line.
fn build_family_repo_maps(
    conn: &Connection,
    families: &[family::Family],
) -> Result<(HashMap<String, String>, HashMap<String, PathBuf>)> {
    let mut by_toplevel: HashMap<String, String> = HashMap::new();
    let mut by_name: HashMap<String, PathBuf> = HashMap::new();
    for fam in families {
        if let Some(top) = repo_toplevel_for_family(conn, fam)? {
            by_toplevel
                .entry(top.to_string_lossy().into_owned())
                .or_insert_with(|| fam.name.clone());
            by_name.entry(fam.name.clone()).or_insert(top);
        }
    }
    Ok((by_toplevel, by_name))
}

// ---------------------------------------------------------------------
// loader
// ---------------------------------------------------------------------

#[derive(Debug, Clone, serde::Deserialize)]
struct RawPrompt {
    #[serde(default)]
    display: String,
    #[serde(default)]
    project: String, // a filesystem PATH, attributed via toplevel_of_dir, never by name
    #[serde(default)]
    timestamp: i64,
}

#[derive(Debug, Clone)]
pub struct LoadedPrompt {
    pub family: String,
    pub project_path: String,
    pub display: String,
    pub ts: i64,
    /// Receipt: 1-based line number in `history.jsonl`.
    pub line_no: usize,
    /// Present when this prompt came from B2's immutable intent ledger rather
    /// than `history.jsonl`.
    pub intent_receipt: Option<IntentPromptReceipt>,
}

#[derive(Debug, Clone)]
pub struct IntentPromptReceipt {
    pub session_id: String,
    pub turn: u32,
    pub byte_start: usize,
}

/// Loader: single line-delimited file, window-filtered, family-attributed
/// via [`toplevel_of_dir`] against `family_by_toplevel` (built once by
/// [`build_family_repo_maps`]), intra-channel dedup on `(normalized
/// display, family)`. A line whose `project` resolves to no known family
/// is dropped, not guessed. Deterministic: file order, first wins.
pub fn load_history_prompts(
    history_file: &Path,
    window: (i64, i64),
    family_by_toplevel: &HashMap<String, String>,
) -> Result<Vec<LoadedPrompt>> {
    let raw = std::fs::read_to_string(history_file)
        .with_context(|| format!("reading {}", history_file.display()))?;
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut out = Vec::new();
    for (i, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(p) = serde_json::from_str::<RawPrompt>(line) else {
            continue;
        };
        if p.display.trim().is_empty() || p.project.is_empty() {
            continue;
        }
        let ts = normalize_ts_secs(p.timestamp);
        if ts < window.0 || ts > window.1 {
            continue;
        }
        let Some(fam) = toplevel_of_dir(&p.project).and_then(|t| family_by_toplevel.get(&t)) else {
            continue;
        };
        if !seen.insert((normalize(&p.display), fam.clone())) {
            continue;
        }
        out.push(LoadedPrompt {
            family: fam.clone(),
            project_path: p.project,
            display: p.display,
            ts,
            line_no: i + 1,
            intent_receipt: None,
        });
    }
    Ok(out)
}

fn load_intent_prompts(
    conn: &Connection,
    window: (i64, i64),
    families: &[family::Family],
) -> Result<Vec<LoadedPrompt>> {
    let events = crate::storage::intent_events::list(conn, None, None)?;
    let mut prompts = Vec::new();
    for event in events {
        let eligible = match event.kind {
            crate::transcript::intent_events::IntentEventKind::Abandoned => true,
            crate::transcript::intent_events::IntentEventKind::Correction => {
                event.symbol.is_some()
                    || event.file.is_some()
                    || !extract_targets(&event.quote).is_empty()
            }
            crate::transcript::intent_events::IntentEventKind::Redirect => false,
        };
        if !eligible {
            continue;
        }
        let Some(ts) = crate::temporal::parse_timestamp(&event.ts).map(|value| value.timestamp())
        else {
            continue;
        };
        if ts < window.0 || ts > window.1 {
            continue;
        }
        let Some(family) = family::family_containing(families, &event.project) else {
            continue;
        };
        prompts.push(LoadedPrompt {
            family: family.name.clone(),
            project_path: event.transcript_path.to_string_lossy().into_owned(),
            display: event.quote,
            ts,
            line_no: event.turn as usize,
            intent_receipt: Some(IntentPromptReceipt {
                session_id: event.session_id,
                turn: event.turn,
                byte_start: event.byte_start,
            }),
        });
    }
    Ok(prompts)
}

/// Episode text per family, for dedup-vs-episodes: `(normalized
/// request+completed+next_steps+blockers, request-only token set)`. A
/// prompt already covered by an episode is not new evidence. Real
/// `episode_index` has no `intent`/`todos` columns — `request` is the
/// closest real equivalent to "what was intended", and the other three
/// text columns widen the substring-containment check.
fn episode_texts_for_family(
    conn: &Connection,
    fam: &family::Family,
) -> Result<Vec<(String, HashSet<String>)>> {
    if fam.members.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders: Vec<String> = (1..=fam.members.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "SELECT COALESCE(request,'') || ' ' || COALESCE(completed,'') || ' ' \
                || COALESCE(next_steps,'') || ' ' || COALESCE(blockers,''), \
                COALESCE(request,'') \
         FROM episode_index WHERE project IN ({})",
        placeholders.join(",")
    );
    let params: Vec<&dyn rusqlite::ToSql> = fam
        .members
        .iter()
        .map(|m| m as &dyn rusqlite::ToSql)
        .collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params.as_slice(), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .map(|(all, request)| (normalize(&all), content_tokens(&request)))
        .collect())
}

fn duplicated_by_episode(
    norm_prompt: &str,
    ptoks: &HashSet<String>,
    texts: &[(String, HashSet<String>)],
) -> bool {
    texts.iter().any(|(all, itoks)| {
        all.contains(norm_prompt) || containment(ptoks, itoks) >= EPISODE_DUP_CONTAINMENT
    })
}

// ---------------------------------------------------------------------
// target extraction (pure, schema-independent)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    PathLike,
    Identifier,
}

#[derive(Debug, Clone)]
pub struct ApproachTarget {
    /// VERBATIM slice of the prompt's `display` at `[byte_start, byte_end)`.
    pub phrase: String,
    pub byte_start: usize,
    pub byte_end: usize,
    pub kind: TargetKind,
    /// The pickaxe search key (`None` for a pure path target that resolves
    /// to real files — those use a path-scoped `git log`, not pickaxe).
    pub ident: Option<String>,
}

/// Shared with [`strip_known_ext`] (pass 3) so the basename normalization
/// used by the SATISFIED guard's path scan agrees with the extensions this
/// module already recognizes as source files.
const SRC_EXTS: &[&str] = &[
    ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".md", ".json", ".toml", ".yaml", ".yml",
    ".css", ".html", ".vue", ".svelte",
];

fn is_src_ext(s: &str) -> bool {
    SRC_EXTS.iter().any(|e| s.ends_with(e))
}

fn is_ident(s: &str) -> bool {
    if s.len() < 5 {
        return false;
    }
    if STOP.contains(&s.to_lowercase().as_str()) {
        return false;
    }
    let chars: Vec<char> = s.chars().collect();
    if !chars[0].is_alphabetic() && chars[0] != '_' {
        return false;
    }
    if !chars.iter().all(|c| c.is_alphanumeric() || *c == '_') {
        return false;
    }
    chars.contains(&'_') || chars[1..].iter().any(|c| c.is_uppercase()) || s.len() >= 6
}

fn mk_target(phrase: String, s: usize, e: usize) -> Option<ApproachTarget> {
    let kind = if phrase.contains('/') || is_src_ext(&phrase) {
        TargetKind::PathLike
    } else if is_ident(&phrase) {
        TargetKind::Identifier
    } else {
        return None;
    };
    let ident = if kind == TargetKind::PathLike {
        None
    } else {
        Some(phrase.clone())
    };
    Some(ApproachTarget {
        phrase,
        byte_start: s,
        byte_end: e,
        kind,
        ident,
    })
}

/// Deterministic extraction: backticked spans first, then whitespace
/// tokens. Byte offsets are exact — they are the prompt receipt.
pub(crate) fn extract_targets(display: &str) -> Vec<ApproachTarget> {
    let mut out = Vec::new();
    let bytes = display.as_bytes();
    let mut start: Option<usize> = None;
    for (i, b) in bytes.iter().enumerate() {
        match (*b, start) {
            (b'`', None) => start = Some(i),
            (b'`', Some(s)) => {
                if i > s + 1 {
                    if let Some(t) = mk_target(display[s + 1..i].to_string(), s + 1, i) {
                        out.push(t);
                    }
                }
                start = None;
            }
            _ => {}
        }
    }
    let mut off = 0usize;
    for tok in display.split_whitespace() {
        let s = display[off..].find(tok).map(|p| off + p).unwrap_or(off);
        let e = s + tok.len();
        off = e;
        let clean = tok.trim_matches(|c: char| {
            !(c.is_alphanumeric() || c == '_' || c == '/' || c == '.' || c == '-')
        });
        if clean.len() >= 4 && !out.iter().any(|t: &ApproachTarget| t.phrase == clean) {
            let clean_start = s + tok.find(clean).unwrap_or(0);
            let clean_end = clean_start + clean.len();
            if let Some(t) = mk_target(clean.to_string(), clean_start, clean_end) {
                out.push(t);
            }
        }
        if out.len() >= MAX_TARGETS_PER_PROMPT {
            break;
        }
    }
    out
}

// ---------------------------------------------------------------------
// repo index + git legs (small per-module git helpers, per this
// codebase's own established convention — see family.rs's module doc)
// ---------------------------------------------------------------------

fn git_at(repo_root: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new("git");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&k);
        }
    }
    cmd.arg("-C").arg(repo_root);
    cmd
}

/// Fail-soft: a failing/erroring git invocation yields empty output rather
/// than aborting the whole family scan over one unresolvable target.
fn git_stdout(repo_root: &Path, args: &[&str]) -> String {
    let Ok(out) = git_at(repo_root).args(args).output() else {
        return String::new();
    };
    if !out.status.success() {
        return String::new();
    }
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn head_oid(repo_root: &Path) -> Result<String> {
    let s = git_stdout(repo_root, &["rev-parse", "HEAD"]);
    let s = s.trim().to_string();
    if s.is_empty() {
        anyhow::bail!("no resolvable HEAD in {}", repo_root.display());
    }
    Ok(s)
}

fn commit_time(repo_root: &Path, oid: &str) -> Result<i64> {
    let s = git_stdout(repo_root, &["show", "-s", "--format=%ct", oid]);
    s.trim().parse::<i64>().with_context(|| {
        format!(
            "parsing committer time for {oid} in {}",
            repo_root.display()
        )
    })
}

fn ls_files(repo_root: &Path) -> Vec<String> {
    git_stdout(repo_root, &["ls-files"])
        .lines()
        .map(str::to_string)
        .collect()
}

pub struct RepoIndex {
    pub files: Vec<String>,
}

impl RepoIndex {
    pub fn load(repo: &Path) -> Result<Self> {
        Ok(Self {
            files: ls_files(repo),
        })
    }
}

fn resolve_paths(idx: &RepoIndex, t: &ApproachTarget) -> Vec<String> {
    let p = &t.phrase;
    let exact: Vec<String> = idx
        .files
        .iter()
        .filter(|f| f.as_str() == p)
        .cloned()
        .collect();
    if !exact.is_empty() {
        return exact.into_iter().take(10).collect();
    }
    let mut hits: Vec<String> = idx
        .files
        .iter()
        .filter(|f| f.ends_with(p.as_str()) || f.ends_with(&format!("/{p}")))
        .cloned()
        .collect();
    if hits.is_empty() {
        hits = idx
            .files
            .iter()
            .filter(|f| {
                [
                    ".rs", ".ts", ".tsx", ".js", ".jsx", ".md", ".vue", ".svelte",
                ]
                .iter()
                .find_map(|e| f.strip_suffix(e))
                .map(|s| s == p.as_str() || s.ends_with(p.as_str()))
                .unwrap_or(false)
            })
            .cloned()
            .collect();
    }
    hits.truncate(10);
    hits
}

fn ident_of_path(p: &str) -> Option<String> {
    let seg = p.rsplit('/').next()?;
    let stem = seg.split('.').next()?;
    (stem.len() >= 4 && stem.chars().all(|c| c.is_alphanumeric() || c == '_'))
        .then(|| stem.to_string())
}

pub enum GitLeg {
    /// No delivery signal found anywhere. `last_touch` is the newest PRE-T
    /// commit reachable from the pinned HEAD that touched the target, when
    /// one exists (receipt only — not itself evidence of anything).
    Negative { last_touch: Option<(String, i64)> },
    /// Git shipped it (leg A basename scan, or leg B all-history pickaxe):
    /// the guard fires, this is NOT a dream. `oid`/`ts` are `None` for a
    /// basename-scan hit (the set carries no per-entry commit — see
    /// [`delivered_basenames`]) and `Some` for a pickaxe hit.
    Satisfied {
        oid: Option<String>,
        ts: Option<i64>,
    },
}

fn parse_hct(line: &str) -> Option<(String, i64)> {
    let (oid, t) = line.split_once(' ')?;
    Some((oid.to_string(), t.parse().ok()?))
}

/// Pass 3 hardening: strip a known source extension (case already lowered
/// by the caller) off a basename, so a target and a delivered file compare
/// equal across an extension change (`widget.js` renamed to `widget.ts`
/// still matches). A basename with no recognized extension (bare
/// identifiers) is returned unchanged.
fn strip_known_ext(lower: &str) -> String {
    for ext in SRC_EXTS {
        if let Some(stem) = lower.strip_suffix(ext) {
            if !stem.is_empty() {
                return stem.to_string();
            }
        }
    }
    lower.to_string()
}

/// Normalize a git-reported path (always `/`-separated) down to a
/// comparable basename: last path segment, lowercased, known extension
/// stripped. `None` for an empty/blank line (`git log --name-only` emits
/// blank separator lines between commits).
fn normalize_basename(path: &str) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    let seg = trimmed.rsplit('/').next()?;
    if seg.is_empty() {
        return None;
    }
    Some(strip_known_ext(&seg.to_lowercase()))
}

/// The SAME normalization applied to a candidate target, so it can be
/// looked up in [`delivered_basenames`]'s set. `PathLike` uses the
/// phrase's own basename; `Identifier` has no path separator, so the
/// phrase itself (lowercased, extension-stripped defensively) stands in
/// for one — this is what lets a target phrase `widget.rs` match a
/// delivered `Widget.tsx`, and an identifier target `WidgetCard` match a
/// delivered `widgetcard.tsx`.
fn target_basename_for_scan(t: &ApproachTarget) -> Option<String> {
    match t.kind {
        TargetKind::PathLike => normalize_basename(&t.phrase),
        TargetKind::Identifier => {
            let stem = strip_known_ext(&t.phrase.to_lowercase());
            (!stem.is_empty()).then_some(stem)
        }
    }
}

/// Pass 3 hardening (leg A — the "normalized path scan"): ONE git call per
/// FAMILY (never per-candidate — the caller invokes this once, before its
/// per-prompt loop, and reuses the result for every target in that
/// family), listing every basename that has EVER appeared in ANY commit on
/// ANY branch, normalized via [`normalize_basename`]. Complements leg B's
/// content pickaxe below: this catches a file that shipped under the
/// exact name/rename the prompt asked for even when its CONTENT never
/// literally contained the identifier as a token (an empty scaffold file,
/// a pure rename).
fn delivered_basenames(repo: &Path) -> HashSet<String> {
    git_stdout(repo, &["log", "--all", "--name-only", "--format="])
        .lines()
        .filter_map(normalize_basename)
        .collect()
}

/// Pass 3 hardening (leg B): the widened content pickaxe. `--all` (every
/// branch, every ref) and NO `--since` (every point in history, not just
/// after the prompt) — ANY commit that ever introduced or removed the
/// identifier, anywhere, marks the guard SATISFIED. The pinned HEAD is no
/// longer passed here at all; it is used ONLY as the horizon receipt in
/// the rendered claim (see [`build_candidate`]), never to scope this
/// search — see the module doc's "Pass 3" section for why this is
/// deliberately biased toward false-SATISFIED over false-abandoned.
fn pickaxe_leg_all_history(repo: &Path, ident: &str) -> GitLeg {
    let out = git_stdout(repo, &["log", "--all", "-S", ident, "--format=%H %ct"]);
    if let Some((oid, cts)) = out.lines().next().and_then(parse_hct) {
        return GitLeg::Satisfied {
            oid: Some(oid),
            ts: Some(cts),
        };
    }
    GitLeg::Negative { last_touch: None }
}

/// Git legs A (basename scan) and B (all-history pickaxe) for one target.
/// Leg C (subagent transcripts) is checked separately by the caller — see
/// [`generate_abandonment_candidates`] — since it needs the family's
/// subagent-transcript index, not a git repo.
fn git_leg(
    repo: &Path,
    head: &str,
    t: &ApproachTarget,
    idx: &RepoIndex,
    delivered: &HashSet<String>,
    ts: i64,
) -> GitLeg {
    // Leg A: normalized basename scan against the per-family delivered set.
    if let Some(basename) = target_basename_for_scan(t) {
        if delivered.contains(&basename) {
            return GitLeg::Satisfied {
                oid: None,
                ts: None,
            };
        }
    }

    // Leg B: all-branches, whole-history content pickaxe. `PathLike`
    // derives its pickaxe identifier the same way the old per-target
    // fallback did (basename stem via `ident_of_path`) — kept here as a
    // second, content-based signal even though leg A above already covers
    // the common "file present somewhere" case, because a target whose
    // literal identifier appears in a DIFFERENTLY-named file's content
    // (e.g. a helper function extracted into a differently-named module)
    // is still real delivery leg A's path-only view can't see.
    let pickaxe_ident = match t.kind {
        TargetKind::Identifier => t.ident.clone(),
        TargetKind::PathLike => ident_of_path(&t.phrase),
    };
    if let Some(ident) = pickaxe_ident {
        if let GitLeg::Satisfied { oid, ts } = pickaxe_leg_all_history(repo, &ident) {
            return GitLeg::Satisfied { oid, ts };
        }
    }

    // Negative: a pre-T last-touch receipt for a path-like target that
    // currently resolves to a real file — informational only, not itself
    // evidence (see the struct doc).
    let last_touch = if t.kind == TargetKind::PathLike {
        let paths = resolve_paths(idx, t);
        if paths.is_empty() {
            None
        } else {
            let until = format!("--until=@{ts}");
            let mut pre_args: Vec<String> = vec![
                "log".into(),
                "-1".into(),
                "--format=%H %ct".into(),
                until,
                head.to_string(),
                "--".into(),
            ];
            pre_args.extend(paths.iter().cloned());
            let pre_refs: Vec<&str> = pre_args.iter().map(String::as_str).collect();
            git_stdout(repo, &pre_refs)
                .lines()
                .next()
                .and_then(parse_hct)
        }
    } else {
        None
    };
    GitLeg::Negative { last_touch }
}

/// The identifier used for the subagent-citation leg (both the SATISFIED
/// guard's leg C and persisted provenance): the identifier itself for an
/// `Identifier` target, the same basename-stem `ident_of_path` derives for
/// a `PathLike` target (mirrors leg B's own pickaxe-identifier choice
/// above, so all three legs judge the same target the same way).
fn target_symbol(t: &ApproachTarget) -> Option<String> {
    match t.kind {
        TargetKind::Identifier => t.ident.clone(),
        TargetKind::PathLike => ident_of_path(&t.phrase),
    }
}

/// The file passed to [`subagent_citation::cite_subagents_for_symbol`]'s
/// basename-matching gate: the target's own path for `PathLike` (real
/// matching), empty for `Identifier` (no path to match against — see the
/// module doc's residual note: an identifier target's subagent-citation
/// leg is honestly a no-op by this construction, never a false negative
/// dressed up as a real check).
fn target_file_for_citation(t: &ApproachTarget) -> String {
    match t.kind {
        TargetKind::PathLike => t.phrase.clone(),
        TargetKind::Identifier => String::new(),
    }
}

/// One family's subagent-transcript index, built ONCE per family (never
/// per-candidate): every session recorded against this family's member
/// project keys in `episode_index`, resolved to its dash-encoded Claude
/// project directory via the SAME fail-closed lookup
/// (`compose::project_dir_for_parent`) build 1's supersession/unfinished
/// provenance already uses, paired with that session's own `ts` when it
/// parses.
///
/// `entries` (ts-sorted) backs the SATISFIED-guard's leg C
/// ([`transcripts_at_or_after`]), restricted to sessions AT OR AFTER a
/// given prompt's own timestamp: a subagent session that only exists
/// BEFORE the prompt cannot be evidence the specific request was later
/// picked up. `all_transcripts` is UNRESTRICTED and backs the PERSISTED
/// provenance attachment instead ([`build_candidate_evidence`]), which is
/// a mechanical bar-clause audit ("does this dream draw on ANY subagent
/// work"), not a delivery-timing claim — an earlier, unrelated subagent
/// touch of the same identifier still legitimately corroborates the dream
/// card even though it did not (and, being pre-prompt, structurally could
/// not) satisfy the guard. This asymmetry is intentional, not an
/// oversight: it is what lets a genuinely-fired abandonment candidate
/// still carry `bar_clause_met = true` when the evidence for it predates
/// the prompt, while never letting POST-prompt subagent work (real
/// evidence the request was answered) both satisfy the guard AND still
/// count as an "abandoned" dream.
struct FamilySubagentIndex {
    entries: Vec<(i64, Vec<PathBuf>)>,
    all_transcripts: Vec<PathBuf>,
    sessions: Vec<String>,
}

fn family_sessions(
    conn: &Connection,
    fam: &family::Family,
) -> Result<Vec<(String, Option<String>)>> {
    if fam.members.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders: Vec<String> = (1..=fam.members.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "SELECT session_id, MIN(ts) FROM episode_index WHERE project IN ({}) GROUP BY session_id",
        placeholders.join(",")
    );
    let params: Vec<&dyn rusqlite::ToSql> = fam
        .members
        .iter()
        .map(|m| m as &dyn rusqlite::ToSql)
        .collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params.as_slice(), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Fail-soft/fail-open on missing evidence, never a guess: no
/// `projects_root` (a caller that doesn't want the subagent leg at all,
/// e.g. a unit test) yields an empty index, which makes leg C and the
/// persisted-provenance citations both vacuously empty — never a panic,
/// never a false SATISFIED.
fn build_family_subagent_index(
    conn: &Connection,
    fam: &family::Family,
    projects_root: Option<&Path>,
) -> Result<FamilySubagentIndex> {
    let Some(projects_root) = projects_root else {
        return Ok(FamilySubagentIndex {
            entries: Vec::new(),
            all_transcripts: Vec::new(),
            sessions: Vec::new(),
        });
    };
    let sessions = family_sessions(conn, fam)?;
    let mut entries = Vec::new();
    let mut all = Vec::new();
    let mut session_ids = Vec::new();
    for (session_id, ts_raw) in &sessions {
        session_ids.push(session_id.clone());
        let Some(project_dir) =
            super::compose::project_dir_for_parent(conn, session_id, projects_root)?
        else {
            continue;
        };
        let transcripts = subagent_citation::subagent_transcripts_for_session(
            projects_root,
            &project_dir,
            session_id,
        );
        if transcripts.is_empty() {
            continue;
        }
        all.extend(transcripts.iter().cloned());
        if let Some(ts) = ts_raw.as_deref().and_then(crate::temporal::parse_timestamp) {
            entries.push((ts.timestamp(), transcripts));
        }
    }
    entries.sort_by_key(|(ts, _)| *ts);
    Ok(FamilySubagentIndex {
        entries,
        all_transcripts: all,
        sessions: session_ids,
    })
}

/// Every transcript belonging to a family session whose own `ts` is `>=
/// prompt_ts` — the guard leg C population (see [`FamilySubagentIndex`]'s
/// doc for why this is time-restricted while persisted provenance is not).
fn transcripts_at_or_after(index: &FamilySubagentIndex, prompt_ts: i64) -> Vec<PathBuf> {
    index
        .entries
        .iter()
        .filter(|(ts, _)| *ts >= prompt_ts)
        .flat_map(|(_, paths)| paths.iter().cloned())
        .collect()
}

/// Guard leg C: did any (post-prompt) family subagent transcript AUTHOR
/// (a real edit, never a mere text/Grep/Read mention — see
/// `subagent_citation`'s own doc) the target? `false` whenever
/// [`target_symbol`] can't derive an identifier at all — never a guess.
fn subagent_leg(transcripts: &[PathBuf], t: &ApproachTarget) -> bool {
    let Some(symbol) = target_symbol(t) else {
        return false;
    };
    let file = target_file_for_citation(t);
    !subagent_citation::cite_subagents_for_symbol(transcripts, &symbol, &file).is_empty()
}

/// Persisted provenance for a SURVIVING candidate: a mechanical bar-clause
/// audit over ALL of the family's subagent transcripts (unrestricted by
/// time, unlike leg C above — see [`FamilySubagentIndex`]'s doc for why
/// that asymmetry is deliberate). `parent_sessions` is always the family's
/// session set: `history.jsonl` carries no `sessionId` field (see the
/// module doc's field list), so "the abandoned prompt's own session" is
/// never resolvable here, and this always falls back to the family, per
/// the build spec's own fallback clause.
fn build_candidate_evidence(
    index: &FamilySubagentIndex,
    t: &ApproachTarget,
) -> DreamCitationEvidence {
    let citations = match target_symbol(t) {
        Some(symbol) => subagent_citation::cite_subagents_for_symbol(
            &index.all_transcripts,
            &symbol,
            &target_file_for_citation(t),
        ),
        None => Vec::new(),
    };
    DreamCitationEvidence::audited(index.sessions.clone(), citations)
}

// ---------------------------------------------------------------------
// generator
// ---------------------------------------------------------------------

/// A lightweight, module-local receipt record for an
/// [`AbandonmentCandidate`] — NOT the same type as
/// [`super::claim_resolution::QuoteSlot`] or [`super::pairs::PairReceipt`]
/// (those model different pass-1 shapes for claims that DO land in
/// `dream_relations`). This one stays report-only in this pass — see the
/// module doc's reconciliation note on why `AbandonmentCandidate` is not
/// persisted here.
#[derive(Debug, Clone)]
pub struct Receipt {
    pub kind: &'static str,
    pub oid: Option<String>,
    pub path: Option<String>,
    pub byte_start: Option<usize>,
    pub byte_end: Option<usize>,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct AbandonmentCandidate {
    pub family: String,
    pub prompt: LoadedPrompt,
    pub target: ApproachTarget,
    /// Horizon receipt: the repo HEAD this family was judged against.
    pub head_oid: String,
    pub claim: String,
    pub receipts: Vec<Receipt>,
    /// Pass 3: mechanically-audited subagent-citation provenance, attached
    /// at generation time and persisted verbatim by
    /// `compose::persist_abandonment_candidates` — see
    /// [`build_candidate_evidence`]'s doc for its (unrestricted-by-time)
    /// scope.
    pub citation_evidence: DreamCitationEvidence,
}

#[derive(Debug, Default)]
pub struct AbandonmentReport {
    pub prompts_loaded: usize,
    pub dedup_vs_episodes: usize,
    pub min_age_skipped: usize,
    pub no_target: usize,
    pub recurrence_skipped: usize,
    pub satisfied_skipped: usize,
    /// Subset of `satisfied_skipped` caused specifically by guard leg C
    /// (a post-prompt family subagent transcript authoring the target) —
    /// broken out for observability, since it is a distinct evidence
    /// channel from the git-based legs A/B.
    pub subagent_satisfied_skipped: usize,
    pub git_budget_exhausted: usize,
    pub candidates: Vec<AbandonmentCandidate>,
}

#[allow(clippy::too_many_arguments)]
fn build_candidate(
    p: &LoadedPrompt,
    t: &ApproachTarget,
    head: &str,
    head_t: i64,
    last_touch: &Option<(String, i64)>,
    later_count: usize,
    citation_evidence: DreamCitationEvidence,
) -> AbandonmentCandidate {
    let target_desc = if t.kind == TargetKind::PathLike {
        format!("`{}`", t.phrase)
    } else {
        format!("anything matching `{}`", t.phrase)
    };
    let claim = format!(
        "On {}, you asked: \"{}\" — and as of {} ({}) no commit after that prompt touches {}, \
         and none of the {} later prompts in this project revisit it. The request appears to \
         have been silently dropped.",
        iso_date(p.ts),
        t.phrase,
        head,
        iso_date(head_t),
        target_desc,
        later_count
    );
    let prompt_receipt = match &p.intent_receipt {
        Some(intent) => Receipt {
            kind: "intent_event",
            oid: None,
            path: Some(p.project_path.clone()),
            byte_start: Some(intent.byte_start.saturating_add(t.byte_start)),
            byte_end: Some(intent.byte_start.saturating_add(t.byte_end)),
            detail: format!(
                "intent_events session {} turn {} @ {} (unix {})",
                intent.session_id,
                intent.turn,
                iso_date(p.ts),
                p.ts
            ),
        },
        None => Receipt {
            kind: "prompt",
            oid: None,
            path: Some(p.project_path.clone()),
            byte_start: Some(t.byte_start),
            byte_end: Some(t.byte_end),
            detail: format!(
                "history.jsonl line {} @ {} (unix {})",
                p.line_no,
                iso_date(p.ts),
                p.ts
            ),
        },
    };
    let mut receipts = vec![
        prompt_receipt,
        Receipt {
            kind: "git_oid",
            oid: Some(head.to_string()),
            path: None,
            byte_start: None,
            byte_end: None,
            detail: format!("horizon: repo head at run time ({})", iso_date(head_t)),
        },
    ];
    if let Some((oid, ts)) = last_touch {
        receipts.push(Receipt {
            kind: "git_oid",
            oid: Some(oid.clone()),
            path: None,
            byte_start: None,
            byte_end: None,
            detail: format!(
                "last commit touching the target BEFORE the prompt ({})",
                iso_date(*ts)
            ),
        });
    }
    receipts.push(Receipt {
        kind: "negative_scan",
        oid: None,
        path: None,
        byte_start: None,
        byte_end: None,
        detail: format!(
            "git log --since=@{} {head} on target => empty; pickaxe -S post-T => 0 hits; \
             prompt recurrence scan over {later_count} later prompts => 0",
            p.ts
        ),
    });
    AbandonmentCandidate {
        family: p.family.clone(),
        prompt: p.clone(),
        target: t.clone(),
        head_oid: head.to_string(),
        claim,
        receipts,
        citation_evidence,
    }
}

/// Silent-abandonment discovery over (prompt, git-negative, subagent-
/// negative, prompt-non-recurrence). Cheap pure-Rust filters run first;
/// git calls are budgeted per family. At most one candidate per prompt
/// (the first qualifying target, in prompt-text order).
///
/// `families` is caller-supplied rather than recomputed internally (pass
/// 3): `cli::handle_backfill` already resolves the exact family set
/// `--project` narrows to, and passing it through here means this stage
/// never scans/git-queries a family the caller didn't ask for. Test
/// callers pass `family::compute_families(conn)`'s own full result when
/// they want every family. `projects_root` is `None` to disable the
/// subagent-citation channel entirely (guard leg C, persisted provenance)
/// — always `Some` in the real pipeline.
pub fn generate_abandonment_candidates(
    conn: &Connection,
    history_file: &Path,
    window: (i64, i64),
    git_call_budget_per_family: usize,
    families: &[family::Family],
    projects_root: Option<&Path>,
) -> Result<AbandonmentReport> {
    let mut rep = AbandonmentReport::default();
    let (by_toplevel, by_name_toplevel) = build_family_repo_maps(conn, families)?;
    let mut prompts = load_history_prompts(history_file, window, &by_toplevel)?;
    prompts.extend(load_intent_prompts(conn, window, families)?);
    rep.prompts_loaded = prompts.len();

    let mut by_family: HashMap<String, Vec<LoadedPrompt>> = HashMap::new();
    for p in prompts {
        by_family.entry(p.family.clone()).or_default().push(p);
    }
    let mut fam_names: Vec<String> = by_family.keys().cloned().collect();
    fam_names.sort();

    for fam_name in fam_names {
        let mut ps = by_family.remove(&fam_name).unwrap_or_default();
        ps.sort_by_key(|p| p.ts); // stable: file order breaks ties -> deterministic
        let Some(repo) = by_name_toplevel.get(&fam_name) else {
            continue;
        };
        let head = head_oid(repo)?;
        let head_t = commit_time(repo, &head)?;
        let idx = RepoIndex::load(repo)?;
        let delivered = delivered_basenames(repo);
        let Some(fam_obj) = families.iter().find(|f| f.name == fam_name) else {
            continue;
        };
        let texts = episode_texts_for_family(conn, fam_obj)?;
        let subagent_index = build_family_subagent_index(conn, fam_obj, projects_root)?;
        let mut budget = git_call_budget_per_family;
        let mut n_candidates = 0usize;

        for (i, p) in ps.iter().enumerate() {
            if n_candidates >= MAX_CANDIDATES_PER_FAMILY {
                break;
            }
            let norm = normalize(&p.display);
            let ptoks = content_tokens(&p.display);
            if duplicated_by_episode(&norm, &ptoks, &texts) {
                rep.dedup_vs_episodes += 1;
                continue;
            }
            if head_t - p.ts < MIN_AGE_DAYS * 86_400 {
                rep.min_age_skipped += 1;
                continue;
            }
            let targets = extract_targets(&p.display);
            if targets.is_empty() {
                rep.no_target += 1;
                continue;
            }
            let later: Vec<&LoadedPrompt> = ps[i + 1..].iter().collect();

            for t in &targets {
                if budget == 0 {
                    rep.git_budget_exhausted += 1;
                    break;
                }
                let ttoks = content_tokens(&t.phrase);
                let rec = later
                    .iter()
                    .filter(|q| {
                        containment(&ttoks, &content_tokens(&q.display)) >= RECURRENCE_CONTAINMENT
                    })
                    .count();
                if rec > 0 {
                    rep.recurrence_skipped += 1;
                    continue; // leg 3 failed
                }
                budget -= 1;
                match git_leg(repo, &head, t, &idx, &delivered, p.ts) {
                    GitLeg::Satisfied { .. } => {
                        rep.satisfied_skipped += 1; // guard: shipped (legs A/B)
                    }
                    GitLeg::Negative { last_touch } => {
                        let scoped = transcripts_at_or_after(&subagent_index, p.ts);
                        if subagent_leg(&scoped, t) {
                            rep.satisfied_skipped += 1; // guard: shipped (leg C)
                            rep.subagent_satisfied_skipped += 1;
                            continue;
                        }
                        let evidence = build_candidate_evidence(&subagent_index, t);
                        rep.candidates.push(build_candidate(
                            p,
                            t,
                            &head,
                            head_t,
                            &last_touch,
                            later.len(),
                            evidence,
                        ));
                        n_candidates += 1;
                        break; // one dream per prompt
                    }
                }
            }
        }
    }
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_git_env(cmd: &mut std::process::Command) {
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(&k);
            }
        }
    }

    fn init_repo(dir: &Path) -> bool {
        std::fs::create_dir_all(dir).unwrap();
        let mut cmd = std::process::Command::new("git");
        strip_git_env(&mut cmd);
        cmd.arg("init").arg("-q").arg(dir);
        cmd.status().map(|s| s.success()).unwrap_or(false)
    }

    fn git(dir: &Path, args: &[&str]) -> bool {
        let mut cmd = std::process::Command::new("git");
        strip_git_env(&mut cmd);
        cmd.arg("-C").arg(dir).args(args);
        cmd.status().map(|s| s.success()).unwrap_or(false)
    }

    fn commit_all(dir: &Path, msg: &str) -> bool {
        git(dir, &["add", "-A"])
            && git(
                dir,
                &[
                    "-c",
                    "user.email=t@example.com",
                    "-c",
                    "user.name=t",
                    "commit",
                    "-q",
                    "-m",
                    msg,
                ],
            )
    }

    fn open_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::storage::migrations::run(&conn).unwrap();
        conn
    }

    fn seed_project_file(conn: &Connection, project: &str, file: &Path) {
        conn.execute(
            "INSERT INTO witness_ledger (project, file, symbol, stamp, tier, at_oid, source_kind) \
             VALUES (?1, ?2, 'foo', 'b3:seed', 'committed', 'deadbeef', 'backfill')",
            rusqlite::params![project, file.to_string_lossy().to_string()],
        )
        .unwrap();
    }

    #[test]
    fn extract_targets_finds_backticked_and_path_like_spans() {
        let targets = extract_targets("please wire up `src/widget.rs` for the new flow");
        assert!(targets.iter().any(|t| t.phrase == "src/widget.rs"));
        assert_eq!(targets[0].byte_start, 16);
        assert_eq!(
            &"please wire up `src/widget.rs` for the new flow"
                [targets[0].byte_start..targets[0].byte_end],
            "src/widget.rs"
        );
    }

    #[test]
    fn extract_targets_trims_punctuation_from_the_receipt_span() {
        let text = "change (src/widget.rs), then continue";
        let target = extract_targets(text)
            .into_iter()
            .find(|target| target.phrase == "src/widget.rs")
            .unwrap();
        assert_eq!(&text[target.byte_start..target.byte_end], "src/widget.rs");
    }

    #[test]
    fn normalize_ts_secs_treats_13_digit_values_as_milliseconds() {
        assert_eq!(normalize_ts_secs(1_759_019_847_017), 1_759_019_847);
        assert_eq!(normalize_ts_secs(1_759_019_847), 1_759_019_847);
    }

    // -----------------------------------------------------------------
    // Required test 1: abandonment fires on git-negative, and is
    // suppressed when git shipped the target (the satisfied-guard).
    // -----------------------------------------------------------------

    #[test]
    fn abandonment_fires_when_git_never_touches_the_named_target() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return; // git unavailable -- fail-soft skip, matches sibling tests
        }
        std::fs::write(repo.join("a.rs"), "fn foo() {}\n").unwrap();
        assert!(commit_all(&repo, "seed"), "seed commit must succeed");

        let conn = open_conn();
        seed_project_file(&conn, "proj", &repo.join("a.rs"));

        let families = family::compute_families(&conn).unwrap();
        // F6 fix (Codex review pass 1 / main-thread finding): the family
        // is now DISPLAY-named after the repo's own toplevel basename
        // (here, the temp git repo directory literally named "repo") when
        // one resolved, not the raw project key ("proj") -- resolve the
        // expected name dynamically rather than hardcoding the old
        // shortest-key convention.
        let expected_family_name = family::family_containing(&families, "proj")
            .expect("proj resolves to a family")
            .name
            .clone();
        let (by_top, by_name) = build_family_repo_maps(&conn, &families).unwrap();
        assert!(
            by_name.contains_key(&expected_family_name),
            "family must resolve to the seeded repo"
        );

        let old_ts_ms = (chrono::Utc::now().timestamp() - 20 * 86_400) * 1000;
        let history = tmp.path().join("history.jsonl");
        std::fs::write(
            &history,
            format!(
                "{{\"display\":\"please add `widget.rs`\",\"project\":\"{}\",\"timestamp\":{old_ts_ms}}}\n",
                repo.to_string_lossy()
            ),
        )
        .unwrap();

        let window = (0i64, chrono::Utc::now().timestamp() + 86_400);
        let rep =
            generate_abandonment_candidates(&conn, &history, window, 50, &families, None).unwrap();
        assert_eq!(rep.prompts_loaded, 1);
        assert_eq!(
            rep.candidates.len(),
            1,
            "widget.rs was never touched by any commit -- must fire. report: {rep:?}"
        );
        assert_eq!(rep.candidates[0].family, expected_family_name);
        assert!(rep.candidates[0].claim.contains("widget.rs"));
        assert!(
            !rep.candidates[0].citation_evidence.bar_clause_met,
            "no subagent fixture is present -- bar clause must be honestly false"
        );
        let _ = by_top; // exercised via generate_abandonment_candidates above
    }

    #[test]
    fn correction_intent_event_is_a_receipted_abandonment_evidence_leg() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return;
        }
        std::fs::write(repo.join("a.rs"), "fn foo() {}\n").unwrap();
        assert!(commit_all(&repo, "seed"));
        let conn = open_conn();
        seed_project_file(&conn, "proj", &repo.join("a.rs"));
        let quote = "No, use `NeverShippedParser` instead";
        let transcript = tmp.path().join("session.jsonl");
        std::fs::write(&transcript, quote).unwrap();
        let old = chrono::Utc::now() - chrono::Duration::days(20);
        crate::storage::intent_events::insert(
            &conn,
            &[crate::transcript::intent_events::IntentEvent {
                session_id: "session-1".into(),
                project: "proj".into(),
                turn: 9,
                kind: crate::transcript::intent_events::IntentEventKind::Correction,
                quote: quote.into(),
                transcript_path: transcript.clone(),
                byte_start: 0,
                byte_end: quote.len(),
                prior_claim: "I used the first parser".into(),
                symbol: Some("NeverShippedParser".into()),
                file: None,
                classifier_hash: "test".into(),
                ts: old.to_rfc3339(),
            }],
        )
        .unwrap();
        let history = tmp.path().join("history.jsonl");
        std::fs::write(&history, "").unwrap();
        let families = family::compute_families(&conn).unwrap();

        let report = generate_abandonment_candidates(
            &conn,
            &history,
            (0, chrono::Utc::now().timestamp()),
            50,
            &families,
            None,
        )
        .unwrap();

        assert_eq!(report.prompts_loaded, 1);
        assert_eq!(report.candidates.len(), 1);
        let receipt = report.candidates[0]
            .receipts
            .iter()
            .find(|receipt| receipt.kind == "intent_event")
            .expect("candidate must retain the intent-event receipt");
        assert_eq!(receipt.path.as_deref(), transcript.to_str());
        let start = receipt.byte_start.unwrap();
        let end = receipt.byte_end.unwrap();
        assert_eq!(
            &std::fs::read(&transcript).unwrap()[start..end],
            b"NeverShippedParser"
        );
    }

    #[test]
    fn abandonment_is_suppressed_when_git_later_ships_the_target() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return;
        }
        std::fs::write(repo.join("a.rs"), "fn foo() {}\n").unwrap();
        assert!(commit_all(&repo, "seed"), "seed commit must succeed");

        let conn = open_conn();
        seed_project_file(&conn, "proj", &repo.join("a.rs"));
        let families = family::compute_families(&conn).unwrap();

        let old_ts_ms = (chrono::Utc::now().timestamp() - 20 * 86_400) * 1000;
        let history = tmp.path().join("history.jsonl");
        std::fs::write(
            &history,
            format!(
                "{{\"display\":\"please add `widget.rs`\",\"project\":\"{}\",\"timestamp\":{old_ts_ms}}}\n",
                repo.to_string_lossy()
            ),
        )
        .unwrap();

        // Git DOES ship it: a later commit (real wall-clock "now", which is
        // after the backdated prompt) introduces the target.
        std::fs::write(repo.join("widget.rs"), "widget implementation\n").unwrap();
        assert!(
            commit_all(&repo, "add widget"),
            "widget commit must succeed"
        );

        let window = (0i64, chrono::Utc::now().timestamp() + 86_400);
        let rep =
            generate_abandonment_candidates(&conn, &history, window, 50, &families, None).unwrap();
        assert_eq!(rep.prompts_loaded, 1);
        assert_eq!(
            rep.candidates.len(),
            0,
            "widget.rs shipped after the prompt -- the satisfied-guard must suppress. report: {rep:?}"
        );
        assert_eq!(rep.satisfied_skipped, 1);
    }

    // -----------------------------------------------------------------
    // Required test (a): a candidate whose identifier is introduced on a
    // NON-checked-out branch is now SATISFIED (not abandoned) under the
    // `--all` widening -- the exact hardening this build adds. Under the
    // OLD single-ref/since-T pickaxe, this identifier is unreachable from
    // the pinned HEAD and the guard would have wrongly fired.
    // -----------------------------------------------------------------

    #[test]
    fn abandonment_is_suppressed_when_the_identifier_lands_on_a_non_checked_out_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return; // git unavailable -- fail-soft skip
        }
        std::fs::write(repo.join("a.rs"), "fn foo() {}\n").unwrap();
        assert!(commit_all(&repo, "seed"), "seed commit must succeed");

        // Introduce the identifier on a SIDE branch that is never merged
        // or checked back into the branch HEAD currently points at.
        assert!(git(&repo, &["checkout", "-q", "-b", "side-branch"]));
        std::fs::write(repo.join("feature.rs"), "fn WidgetFeatureFlag() {}\n").unwrap();
        assert!(commit_all(&repo, "side work"), "side commit must succeed");
        assert!(
            git(&repo, &["checkout", "-q", "-"]),
            "must return to the original branch"
        );

        let conn = open_conn();
        seed_project_file(&conn, "proj", &repo.join("a.rs"));
        let families = family::compute_families(&conn).unwrap();

        let old_ts_ms = (chrono::Utc::now().timestamp() - 20 * 86_400) * 1000;
        let history = tmp.path().join("history.jsonl");
        std::fs::write(
            &history,
            format!(
                "{{\"display\":\"please add `WidgetFeatureFlag`\",\"project\":\"{}\",\"timestamp\":{old_ts_ms}}}\n",
                repo.to_string_lossy()
            ),
        )
        .unwrap();

        let window = (0i64, chrono::Utc::now().timestamp() + 86_400);
        let rep =
            generate_abandonment_candidates(&conn, &history, window, 50, &families, None).unwrap();
        assert_eq!(rep.prompts_loaded, 1);
        assert_eq!(
            rep.candidates.len(),
            0,
            "identifier landed on a side branch -- the --all pickaxe must catch it. report: {rep:?}"
        );
        assert_eq!(rep.satisfied_skipped, 1);
    }

    // -----------------------------------------------------------------
    // Required test (b): a genuinely-absent identifier still fires under
    // the widened guard (regression check -- the hardening must not make
    // the guard trivially always-satisfied).
    // -----------------------------------------------------------------

    #[test]
    fn abandonment_still_fires_for_a_genuinely_absent_identifier() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return;
        }
        std::fs::write(repo.join("a.rs"), "fn foo() {}\n").unwrap();
        assert!(commit_all(&repo, "seed"), "seed commit must succeed");

        let conn = open_conn();
        seed_project_file(&conn, "proj", &repo.join("a.rs"));
        let families = family::compute_families(&conn).unwrap();

        let old_ts_ms = (chrono::Utc::now().timestamp() - 20 * 86_400) * 1000;
        let history = tmp.path().join("history.jsonl");
        std::fs::write(
            &history,
            format!(
                "{{\"display\":\"please wire up `NeverShippedFeature`\",\"project\":\"{}\",\"timestamp\":{old_ts_ms}}}\n",
                repo.to_string_lossy()
            ),
        )
        .unwrap();

        let window = (0i64, chrono::Utc::now().timestamp() + 86_400);
        let rep =
            generate_abandonment_candidates(&conn, &history, window, 50, &families, None).unwrap();
        assert_eq!(
            rep.candidates.len(),
            1,
            "identifier never introduced anywhere -- must still fire. report: {rep:?}"
        );
        assert!(rep.candidates[0].claim.contains("NeverShippedFeature"));
    }

    // -----------------------------------------------------------------
    // Required test (d): provenance attaches when a fixture subagent
    // transcript authors the target. The transcript's own session predates
    // the prompt, so guard leg C (post-prompt only) correctly does NOT
    // suppress the candidate -- it still fires -- while the UNRESTRICTED
    // persisted-provenance scope still finds and attaches that earlier
    // transcript's citation, so `bar_clause_met` comes back true on a
    // genuinely-fired candidate. See `FamilySubagentIndex`'s doc for why
    // this asymmetry (leg C time-scoped, provenance unrestricted) is the
    // only way both "the guard never wrongly suppresses on pre-prompt
    // noise" and "surviving candidates can still carry real provenance"
    // hold at once.
    // -----------------------------------------------------------------

    #[test]
    fn provenance_attaches_from_a_family_subagent_transcript_that_predates_the_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return;
        }
        std::fs::write(repo.join("a.rs"), "fn foo() {}\n").unwrap();
        assert!(commit_all(&repo, "seed"), "seed commit must succeed");

        let conn = open_conn();
        seed_project_file(&conn, "proj", &repo.join("a.rs"));

        // An older session, resolvable to a project_dir + subagent
        // transcript authoring `feature.rs`, chronologically BEFORE the
        // abandoned prompt below.
        let projects_root = tmp.path().join("claude-projects");
        let project_dir = "-repo";
        let session_id = "sess-old";
        let session_ts = (chrono::Utc::now() - chrono::Duration::days(40)).to_rfc3339();
        conn.execute(
            "INSERT INTO episode_index (episode_id, session_id, project, ts, outcome) \
             VALUES ('ep-old', ?1, 'proj', ?2, 'completed')",
            rusqlite::params![session_id, session_ts],
        )
        .unwrap();
        let session_path = projects_root
            .join(project_dir)
            .join(format!("{session_id}.jsonl"));
        conn.execute(
            "INSERT INTO import_state (file_path, conversation_id, chunks_imported) \
             VALUES (?1, ?2, 0)",
            rusqlite::params![session_path.to_string_lossy().to_string(), session_id],
        )
        .unwrap();
        let subagents_dir = projects_root
            .join(project_dir)
            .join(session_id)
            .join("subagents");
        std::fs::create_dir_all(&subagents_dir).unwrap();
        let transcript_bytes = format!(
            "{}\n",
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "tool-edit",
                        "name": "Edit",
                        "input": {
                            "file_path": "/repo/feature.rs",
                            "old_string": "before",
                            "new_string": "fn feature() {}"
                        }
                    }]
                }
            })
        );
        std::fs::write(subagents_dir.join("agent-cited.jsonl"), transcript_bytes).unwrap();

        let families = family::compute_families(&conn).unwrap();

        let old_ts_ms = (chrono::Utc::now().timestamp() - 20 * 86_400) * 1000;
        let history = tmp.path().join("history.jsonl");
        std::fs::write(
            &history,
            format!(
                "{{\"display\":\"please wire up `feature.rs` for this\",\"project\":\"{}\",\"timestamp\":{old_ts_ms}}}\n",
                repo.to_string_lossy()
            ),
        )
        .unwrap();

        let window = (0i64, chrono::Utc::now().timestamp() + 86_400);
        let rep = generate_abandonment_candidates(
            &conn,
            &history,
            window,
            50,
            &families,
            Some(&projects_root),
        )
        .unwrap();

        assert_eq!(
            rep.candidates.len(),
            1,
            "the authoring transcript predates the prompt -- leg C must NOT suppress. report: {rep:?}"
        );
        assert_eq!(rep.subagent_satisfied_skipped, 0);
        let evidence = &rep.candidates[0].citation_evidence;
        assert!(
            evidence.bar_clause_met,
            "persisted provenance is unrestricted by time -- the earlier transcript must still attach"
        );
        assert!(!evidence.citations.is_empty());
        assert_eq!(evidence.citations[0].session_id, "cited");
        assert!(evidence
            .provenance
            .parent_sessions
            .contains(&session_id.to_string()));
    }

    // -----------------------------------------------------------------
    // Guard leg C in isolation: a POST-prompt family subagent transcript
    // authoring the target DOES suppress the candidate.
    // -----------------------------------------------------------------

    #[test]
    fn abandonment_is_suppressed_when_a_post_prompt_subagent_transcript_authors_the_target() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        if !init_repo(&repo) {
            return;
        }
        std::fs::write(repo.join("a.rs"), "fn foo() {}\n").unwrap();
        assert!(commit_all(&repo, "seed"), "seed commit must succeed");

        let conn = open_conn();
        seed_project_file(&conn, "proj", &repo.join("a.rs"));

        let projects_root = tmp.path().join("claude-projects");
        let project_dir = "-repo";
        let session_id = "sess-new";
        // AFTER the prompt below.
        let session_ts = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO episode_index (episode_id, session_id, project, ts, outcome) \
             VALUES ('ep-new', ?1, 'proj', ?2, 'completed')",
            rusqlite::params![session_id, session_ts],
        )
        .unwrap();
        let session_path = projects_root
            .join(project_dir)
            .join(format!("{session_id}.jsonl"));
        conn.execute(
            "INSERT INTO import_state (file_path, conversation_id, chunks_imported) \
             VALUES (?1, ?2, 0)",
            rusqlite::params![session_path.to_string_lossy().to_string(), session_id],
        )
        .unwrap();
        let subagents_dir = projects_root
            .join(project_dir)
            .join(session_id)
            .join("subagents");
        std::fs::create_dir_all(&subagents_dir).unwrap();
        let transcript_bytes = format!(
            "{}\n",
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "tool-edit",
                        "name": "Edit",
                        "input": {
                            "file_path": "/repo/feature.rs",
                            "old_string": "before",
                            "new_string": "fn feature() {}"
                        }
                    }]
                }
            })
        );
        std::fs::write(subagents_dir.join("agent-cited.jsonl"), transcript_bytes).unwrap();

        let families = family::compute_families(&conn).unwrap();

        let old_ts_ms = (chrono::Utc::now().timestamp() - 20 * 86_400) * 1000;
        let history = tmp.path().join("history.jsonl");
        std::fs::write(
            &history,
            format!(
                "{{\"display\":\"please wire up `feature.rs` for this\",\"project\":\"{}\",\"timestamp\":{old_ts_ms}}}\n",
                repo.to_string_lossy()
            ),
        )
        .unwrap();

        let window = (0i64, chrono::Utc::now().timestamp() + 86_400);
        let rep = generate_abandonment_candidates(
            &conn,
            &history,
            window,
            50,
            &families,
            Some(&projects_root),
        )
        .unwrap();

        assert_eq!(
            rep.candidates.len(),
            0,
            "a post-prompt subagent transcript authored the target -- leg C must suppress. report: {rep:?}"
        );
        assert_eq!(rep.subagent_satisfied_skipped, 1);
    }

    #[test]
    fn duplicated_by_episode_matches_substring_and_containment() {
        let texts = vec![(
            "fixed the login flow bug".to_string(),
            content_tokens("login flow"),
        )];
        assert!(duplicated_by_episode(
            "fixed the login flow bug",
            &content_tokens("login"),
            &texts
        ));
        assert!(!duplicated_by_episode(
            "totally unrelated prompt text",
            &content_tokens("unrelated prompt"),
            &texts
        ));
    }
}
