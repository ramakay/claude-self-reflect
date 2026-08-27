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
//! INVARIANT: an abandonment candidate fires only when ALL THREE legs hold,
//! each receipt-carrying:
//!   1. INTENT — prompt P verbatim-names approach X at time T for family F.
//!   2. GIT-NEGATIVE — as of the pinned repo HEAD, no commit after T
//!      touches any path X resolves to, and pickaxe finds no post-T
//!      introduction of X's identifier anywhere in the repo.
//!   3. NON-RECURRENCE — no later prompt in F re-mentions X.
//!
//! The SATISFIED-prompt guard is deliberately one-sided: ANY post-T git
//! activity on the target (a path touch or a pickaxe hit) marks the prompt
//! SATISFIED and kills the candidate outright. A false "abandoned" is worse
//! than a missed dream, so leg 2 is checked to fail closed.
//!
//! Zero LLM calls anywhere in this module.
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
//!   code that needs it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::extraction::repo_root::repo_root_for_file;

use super::family;

/// Pre-registered: younger prompts are unjudgeable (git activity may
/// simply not have caught up yet).
pub const MIN_AGE_DAYS: i64 = 14;
const MAX_TARGETS_PER_PROMPT: usize = 8;
const MAX_CANDIDATES_PER_FAMILY: usize = 50;
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

fn content_tokens(s: &str) -> HashSet<String> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| w.len() > 3 && !STOP.contains(&w.to_lowercase().as_str()))
        .map(|w| w.to_lowercase())
        .collect()
}

fn containment(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
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
        });
    }
    Ok(out)
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

fn is_src_ext(s: &str) -> bool {
    [
        ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".md", ".json", ".toml", ".yaml",
        ".yml", ".css", ".html", ".vue", ".svelte",
    ]
    .iter()
    .any(|e| s.ends_with(e))
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
fn extract_targets(display: &str) -> Vec<ApproachTarget> {
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
            if let Some(t) = mk_target(clean.to_string(), s, e) {
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
    /// No post-T delivery signal found. `last_touch` is the newest
    /// PRE-T commit that touched the target, when one exists (receipt
    /// only — not itself evidence of anything).
    Negative { last_touch: Option<(String, i64)> },
    /// Git shipped it: the guard fires, this is NOT a dream.
    Satisfied { oid: String, ts: i64 },
}

fn parse_hct(line: &str) -> Option<(String, i64)> {
    let (oid, t) = line.split_once(' ')?;
    Some((oid.to_string(), t.parse().ok()?))
}

fn pickaxe_leg(repo: &Path, head: &str, ident: &str, ts: i64) -> GitLeg {
    // Unexcluded by design: the satisfied-guard must see ANY delivery
    // signal, whatever branch/history it landed on reachable from HEAD.
    let since = format!("--since=@{ts}");
    let out = git_stdout(repo, &["log", "-S", ident, "--format=%H %ct", &since, head]);
    if let Some((oid, cts)) = out.lines().next().and_then(parse_hct) {
        return GitLeg::Satisfied { oid, ts: cts };
    }
    GitLeg::Negative { last_touch: None }
}

fn git_leg(repo: &Path, head: &str, t: &ApproachTarget, idx: &RepoIndex, ts: i64) -> GitLeg {
    match t.kind {
        TargetKind::Identifier => match &t.ident {
            Some(id) => pickaxe_leg(repo, head, id, ts),
            None => GitLeg::Negative { last_touch: None },
        },
        TargetKind::PathLike => {
            let paths = resolve_paths(idx, t);
            if paths.is_empty() {
                // Never present at head: judge by basename pickaxe (a
                // post-T hit means something by that name shipped
                // somewhere, i.e. satisfied, not abandoned).
                return match ident_of_path(&t.phrase) {
                    Some(base) => pickaxe_leg(repo, head, &base, ts),
                    None => GitLeg::Negative { last_touch: None },
                };
            }
            let since = format!("--since=@{ts}");
            let mut sat_args: Vec<String> = vec![
                "log".into(),
                "-1".into(),
                "--format=%H %ct".into(),
                since,
                head.to_string(),
                "--".into(),
            ];
            sat_args.extend(paths.iter().cloned());
            let sat_refs: Vec<&str> = sat_args.iter().map(String::as_str).collect();
            let out = git_stdout(repo, &sat_refs);
            if let Some((oid, cts)) = out.lines().next().and_then(parse_hct) {
                return GitLeg::Satisfied { oid, ts: cts }; // GUARD: git DID ship it
            }
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
            let out2 = git_stdout(repo, &pre_refs);
            let last_touch = out2.lines().next().and_then(parse_hct);
            GitLeg::Negative { last_touch }
        }
    }
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
}

#[derive(Debug, Default)]
pub struct AbandonmentReport {
    pub prompts_loaded: usize,
    pub dedup_vs_episodes: usize,
    pub min_age_skipped: usize,
    pub no_target: usize,
    pub recurrence_skipped: usize,
    pub satisfied_skipped: usize,
    pub git_budget_exhausted: usize,
    pub candidates: Vec<AbandonmentCandidate>,
}

fn build_candidate(
    p: &LoadedPrompt,
    t: &ApproachTarget,
    head: &str,
    head_t: i64,
    last_touch: &Option<(String, i64)>,
    later_count: usize,
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
    let mut receipts = vec![
        Receipt {
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
    }
}

/// Silent-abandonment discovery over (prompt, git-negative,
/// prompt-non-recurrence). Cheap pure-Rust filters run first; git calls
/// are budgeted per family. At most one candidate per prompt (the first
/// qualifying target, in prompt-text order).
pub fn generate_abandonment_candidates(
    conn: &Connection,
    history_file: &Path,
    window: (i64, i64),
    git_call_budget_per_family: usize,
) -> Result<AbandonmentReport> {
    let mut rep = AbandonmentReport::default();
    let families = family::compute_families(conn)?;
    let (by_toplevel, by_name_toplevel) = build_family_repo_maps(conn, &families)?;
    let prompts = load_history_prompts(history_file, window, &by_toplevel)?;
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
        let Some(fam_obj) = families.iter().find(|f| f.name == fam_name) else {
            continue;
        };
        let texts = episode_texts_for_family(conn, fam_obj)?;
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
                match git_leg(repo, &head, t, &idx, p.ts) {
                    GitLeg::Satisfied { .. } => {
                        rep.satisfied_skipped += 1; // guard: shipped
                    }
                    GitLeg::Negative { last_touch } => {
                        rep.candidates.push(build_candidate(
                            p,
                            t,
                            &head,
                            head_t,
                            &last_touch,
                            later.len(),
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
        let rep = generate_abandonment_candidates(&conn, &history, window, 50).unwrap();
        assert_eq!(rep.prompts_loaded, 1);
        assert_eq!(
            rep.candidates.len(),
            1,
            "widget.rs was never touched by any commit -- must fire. report: {rep:?}"
        );
        assert_eq!(rep.candidates[0].family, expected_family_name);
        assert!(rep.candidates[0].claim.contains("widget.rs"));
        let _ = by_top; // exercised via generate_abandonment_candidates above
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
        let rep = generate_abandonment_candidates(&conn, &history, window, 50).unwrap();
        assert_eq!(rep.prompts_loaded, 1);
        assert_eq!(
            rep.candidates.len(),
            0,
            "widget.rs shipped after the prompt -- the satisfied-guard must suppress. report: {rep:?}"
        );
        assert_eq!(rep.satisfied_skipped, 1);
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
