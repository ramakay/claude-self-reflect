//! `csr-engine dream backfill` / `csr-engine dream drain` — CLI glue for the
//! dream backfill pipeline (`.plans/dream-backfill-design.md` §4, as amended
//! by §8 D11). Storage-only, no `EmbeddingEngine` — every stage this
//! pipeline runs (materialize, unfinished scan, pair generation, rank/gate,
//! adjudicate, verify, compose, drain) reads/writes SQLite directly, same
//! "no `Engine::new()`" pattern [`crate::dream::cli::handle`] already
//! follows for `csr-engine dreams`.
//!
//! # `--stage` (a judgment call — undocumented by the design beyond naming
//! the flag)
//!
//! The design's CLI signature (§4) lists `--stage k` without specifying its
//! semantics. This module implements it as "run through stage k and stop",
//! a debug/resumability aid consistent with the design's own crash-safety
//! goal ("every stage resumable"):
//!
//! - `0`: materialize + aliveness refresh only.
//! - `1`: + Stage 1 unfinished scan.
//! - `2`: + Stage 2-3 pair generation, rank/gate, and persistence
//!   (project-filtered by `--project` when given) — this is exactly what
//!   `--dry-run` already covers, so `--stage 2` without `--dry-run` is the
//!   same work but WITH persistence into `dream_relations` (dry-run's own
//!   `rank::dry_run` already persists too — see its own doc — so the two
//!   are behaviorally identical up to this point; `--stage` exists for
//!   partial REAL runs beyond stage 2). ALSO runs Stage 3a (pass 3: the
//!   `intent_channel` silent-abandonment scan + `dreams_v1` persist) —
//!   deterministic and zero-LLM like Stage 2-3 itself, so it is not gated
//!   behind a separate stage number; it stops being reached only when
//!   `--stage` is `0` or `1`.
//! - `3` or omitted: + Stage 4/5 adjudicate + verify, budgeted by
//!   `--budget-calls`. The full run.
//!
//! `--dry-run` and `--report` are their own complete modes (stages 0-3
//! read-only-ish preview, and the full post-verification queue render,
//! respectively) and ignore `--stage`.

use std::path::Path;

use anyhow::Result;
use chrono::Utc;

use crate::storage::Storage;

use super::{adjudicate, compose, funnel, intent_channel, rank, unfinished};

/// `csr-engine dream backfill`.
#[allow(clippy::too_many_arguments)]
pub fn handle_backfill(
    db_path: &Path,
    project: Option<&str>,
    budget_calls: usize,
    dry_run: bool,
    report: bool,
    stage: Option<u8>,
    funnel_flag: bool,
) -> Result<()> {
    let storage = Storage::open(db_path)?;

    // Pass 2, item 3: the SQL-stage funnel counter — a read-only corpus
    // instrument (no git, no LLM, no `dream_relations` writes), so unlike
    // `--report` it does NOT check `CSR_NO_DREAMING`: it never runs any
    // backfill-pipeline stage, only counts what past runs already left
    // behind. `--project` narrows to that project's family exactly like
    // the real run below does.
    if funnel_flag {
        let families: Vec<super::family::Family> = {
            let all = storage.with_connection(super::family::compute_families)?;
            match project {
                Some(p) => vec![super::family::family_containing(&all, p)
                    .cloned()
                    .unwrap_or_else(|| super::family::Family::single(p))],
                None => all,
            }
        };
        for fam in &families {
            let stages = storage.with_connection(|conn| funnel::sql_stages(conn, fam))?;
            funnel::print_sql_stages(&fam.name, &stages);
        }
        return Ok(());
    }

    // C (conformance review, HIGH): `--report` must refuse under
    // `CSR_NO_DREAMING` like every other backfill stage — it reads
    // `dream_relations`/re-runs `scan_unfinished` (via
    // `compose::build_ranked_queue`), both of which are backfill-pipeline
    // reads, not a neutral inspection command.
    if report {
        if crate::daemon::dream_cadence::dreaming_disabled() {
            println!("dream backfill: disabled (CSR_NO_DREAMING)");
            return Ok(());
        }
        let now = Utc::now();
        let entries = storage.with_connection(|conn| compose::build_ranked_queue(conn, now))?;
        print!(
            "{}",
            compose::render_full_report(&entries, compose::REPORT_PREVIEW_N)
        );
        return Ok(());
    }

    if dry_run {
        let out = storage.with_connection(rank::dry_run)?;
        print!("{}", rank::render_dry_run_report(&out));
        return Ok(());
    }

    if crate::daemon::dream_cadence::dreaming_disabled() {
        println!("dream backfill: disabled (CSR_NO_DREAMING)");
        return Ok(());
    }

    // P5: one `now` captured up front and threaded through both Stage 1 and
    // Stage 2-3 below, so a single `dream backfill` invocation uses exactly
    // one reference time throughout rather than each stage independently
    // sampling the wall clock moments apart.
    let now = Utc::now();

    let (materialize, aliveness) =
        storage.with_connection(crate::storage::dream_backfill::refresh_episode_index)?;
    println!(
        "Stage 0: {} episodes materialized ({} unresolved aliveness)",
        materialize.upserted, aliveness.episodes_unresolved
    );
    if stage == Some(0) {
        return Ok(());
    }

    let unfinished_report =
        storage.with_connection(|conn| unfinished::scan_unfinished_at(conn, now))?;
    println!(
        "Stage 1: {} seeds considered, {} never picked up ({} queue-eligible)",
        unfinished_report.stats.seeds_considered,
        unfinished_report.stats.never_picked_up,
        unfinished_report.queue.len()
    );
    if stage == Some(1) {
        return Ok(());
    }

    // 2026-08-26 design ruling: Stage 2-3 iterates FAMILIES (cross-project
    // corpus), never raw keys. `--project <p>` selects the family containing
    // `p` — falling back to a single-key family when `p` names no known key
    // (an empty gate run, same outcome as the pre-family behavior).
    let families: Vec<super::family::Family> = {
        let all = storage.with_connection(super::family::compute_families)?;
        match project {
            Some(p) => vec![super::family::family_containing(&all, p)
                .cloned()
                .unwrap_or_else(|| super::family::Family::single(p))],
            None => all,
        }
    };
    for family in &families {
        let (relations, stats) =
            storage.with_connection(|conn| rank::gate_family(conn, family, now))?;
        storage.with_connection(|conn| rank::persist_relations(conn, &relations))?;
        println!(
            "Stage 2-3 [{}]: generated {} (ledger {} / relapse {} / era {}) -> \
             queued {} / archived {} / below_floor {} / outside_window {}",
            stats.project,
            stats.generated,
            stats.generated_ledger,
            stats.generated_relapse,
            stats.generated_era,
            stats.queued,
            stats.archived,
            stats.below_floor,
            stats.outside_window
        );
    }
    // Stage 3a (pass 3, new): silent-abandonment candidates mined from
    // `~/.claude/history.jsonl` (`intent_channel`) — see that module's own
    // doc for the guard/legs this checks. Deterministic, zero-LLM, so it
    // runs alongside Stage 2-3 rather than gated behind `--stage 3`
    // (reserved for the LLM adjudicate/verify stage below). Scoped to the
    // SAME `families` list `--project` already narrowed above, so this
    // never scans/git-queries a family the caller didn't ask for.
    // `CSR_NO_DREAMING` is re-checked here (not just at the top of this
    // function) so a toggle mid-process between the two checks still fails
    // closed, matching every other backfill-pipeline read/write.
    if !crate::daemon::dream_cadence::dreaming_disabled() {
        if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
            let history_file = home.join(".claude/history.jsonl");
            if history_file.exists() {
                let projects_root = home.join(".claude/projects");
                let window = (0i64, now.timestamp());
                let report = storage.with_connection(|conn| {
                    intent_channel::generate_abandonment_candidates(
                        conn,
                        &history_file,
                        window,
                        intent_channel::DEFAULT_GIT_BUDGET_PER_FAMILY,
                        &families,
                        Some(projects_root.as_path()),
                    )
                })?;
                let persist_stats =
                    compose::persist_abandonment_candidates(&storage, &report.candidates, now)?;
                println!(
                    "Stage 3a: {} prompt(s) loaded, {} candidate(s) survived the guard \
                     ({} satisfied [{} via subagent], {} recurrence, {} dedup-vs-episode) -> \
                     persisted {} (bar-eligible {}), skipped {} (recent topic)",
                    report.prompts_loaded,
                    report.candidates.len(),
                    report.satisfied_skipped,
                    report.subagent_satisfied_skipped,
                    report.recurrence_skipped,
                    report.dedup_vs_episodes,
                    persist_stats.persisted,
                    persist_stats.bar_eligible,
                    persist_stats.skipped_recent_topic
                );
            }
        }
    }

    if matches!(stage, Some(s) if s <= 2) {
        return Ok(());
    }

    let adjudicate_stats = adjudicate::run_adjudication(&storage, budget_calls)?;
    println!(
        "Stage 4-5: deterministic promoted {} / discarded {}; LLM attempted {} (related {} / unrelated {} / actor_no_reply {} / malformed {}), \
         verify passed {} / failed {}, backlog {}{}",
        adjudicate_stats.deterministic_promoted,
        adjudicate_stats.deterministic_discarded,
        adjudicate_stats.attempted,
        adjudicate_stats.related,
        adjudicate_stats.unrelated,
        adjudicate_stats.actor_no_reply,
        adjudicate_stats.malformed,
        adjudicate_stats.verify_passed,
        adjudicate_stats.verify_failed,
        adjudicate_stats.backlog_after,
        if adjudicate_stats.adjudicator_suspect {
            " [ADJUDICATOR SUSPECT: UNRELATED rate outside the 10-40% healthy band]"
        } else {
            ""
        }
    );
    Ok(())
}

/// `csr-engine dream drain` — compose up to `n` dreams from the ranked
/// queue into `dreams_v1` (design §3 Stage 6 / §4). Nightly cadence calls
/// this on its own schedule (out of this stage's scope — see the module
/// doc); this is the manual/on-demand entry point.
pub fn handle_drain(db_path: &Path, n: usize) -> Result<()> {
    let storage = Storage::open(db_path)?;
    let stats = compose::drain(&storage, n)?;
    if stats.disabled {
        println!("dream backfill drain: disabled (CSR_NO_DREAMING)");
        return Ok(());
    }
    println!(
        "dream backfill drain: {} candidates, {} drained, {} skipped (recent topic within 30d)",
        stats.candidates, stats.drained, stats.skipped_recent_topic
    );
    Ok(())
}
