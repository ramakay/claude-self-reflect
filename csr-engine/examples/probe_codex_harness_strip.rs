//! W3-R acceptance probe: drives the daemon-owned harness-strip batch job
//! (`codex_rollout::harness_strip_batch`) and the versioned restamp
//! (`codex_rollout::restamp_codex_chunk_timestamps`) to completion against a
//! `/tmp` clone, measuring per-batch throughput and reporting the two
//! acceptance queries from the item spec:
//!
//!   1. `select count(*) from (select conversation_id from chunks where
//!      conversation_id like 'codex:%' group by 1 having count(*)>1 and
//!      count(distinct timestamp)=1)` — multi-chunk codex conversations
//!      still collapsed onto one timestamp.
//!   2. Codex conversations whose seq-0 chunk content still starts with a
//!      harness marker.
//!
//! This repo's convention is a `/tmp` CLONE (`VACUUM INTO` + a copy of the
//! index dir), never the live database — refuses to run otherwise.
//!
//!   cargo run --release --example probe_codex_harness_strip -- \
//!       --db /tmp/csr-w3r-clone/csr-engine.db \
//!       --projects-dir /Users/you/.claude/projects \
//!       --batch-size 25 --max-batches 400

use anyhow::Result;
use csr_engine::engine::Engine;
use csr_engine::import::codex_rollout::{
    harness_strip_batch, restamp_codex_chunk_timestamps, HARNESS_STRIP_V2_BATCH_SIZE,
};
use rusqlite::{Connection, OpenFlags};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

const CODEX_HARNESS_MARKERS: &[&str] = &[
    "<recommended_plugins>",
    "# AGENTS.md instructions",
    "<environment_context>",
    "<user_instructions>",
    "<permissions instructions>",
    "<codex_internal_context>",
    "<skills_instructions>",
];

fn collapsed_timestamp_conversations(conn: &Connection) -> Result<i64> {
    conn.query_row(
        "select count(*) from (
            select conversation_id from chunks
            where conversation_id like 'codex:%'
            group by 1
            having count(*) > 1 and count(distinct timestamp) = 1
         )",
        [],
        |r| r.get(0),
    )
    .map_err(Into::into)
}

fn harness_preview_conversations(conn: &Connection) -> Result<i64> {
    let mut stmt = conn
        .prepare("select content from chunks where conversation_id like 'codex:%' and seq = 0")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut count = 0i64;
    for row in rows {
        let content = row?;
        let trimmed = content.trim_start();
        if CODEX_HARNESS_MARKERS
            .iter()
            .any(|marker| trimmed.starts_with(marker))
        {
            count += 1;
        }
    }
    Ok(count)
}

fn main() -> Result<()> {
    let mut db = PathBuf::from("/tmp/csr-w3r-clone/csr-engine.db");
    let mut projects_dir = dirs::home_dir().expect("home dir").join(".claude/projects");
    let mut batch_size = HARNESS_STRIP_V2_BATCH_SIZE;
    let mut max_batches = 10_000usize;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--db" => db = PathBuf::from(args.next().expect("--db needs a value")),
            "--projects-dir" => {
                projects_dir = PathBuf::from(args.next().expect("--projects-dir needs a value"))
            }
            "--batch-size" => {
                batch_size = args
                    .next()
                    .expect("--batch-size needs a value")
                    .parse()
                    .expect("--batch-size must be a number")
            }
            "--max-batches" => {
                max_batches = args
                    .next()
                    .expect("--max-batches needs a value")
                    .parse()
                    .expect("--max-batches must be a number")
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

    println!("=== W3-R codex harness-strip + restamp acceptance probe ===");
    println!("db: {}", db.display());
    println!("batch size: {batch_size}, max batches: {max_batches}");

    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;

    let before_collapsed = collapsed_timestamp_conversations(&conn)?;
    let before_previews = harness_preview_conversations(&conn)?;
    println!("\n--- before ---");
    println!("collapsed-timestamp codex conversations: {before_collapsed}");
    println!("harness-preview codex conversations:      {before_previews}");
    drop(conn);

    let engine = Engine::new(&db, &projects_dir)?;

    println!("\n--- harness-strip batches ---");
    let mut total_chunks = 0usize;
    let mut total_secs = 0.0f64;
    let mut batches_run = 0usize;
    loop {
        if batches_run >= max_batches {
            println!("stopping: max-batches reached");
            break;
        }
        let started = Instant::now();
        let shutdown = AtomicBool::new(false);
        let stats = harness_strip_batch(&engine, batch_size, &shutdown)?;
        let wall = started.elapsed().as_secs_f64();
        batches_run += 1;
        total_chunks += stats.chunks_reimported;
        total_secs += stats.batch_secs;
        println!(
            "batch {batches_run:>4}: considered={:>5} reimported={:>3} missing_file={:>3} \
             failed={:>2} chunks={:>5} remaining={:>5} batch_secs={:.2} wall_secs={:.2} done={}",
            stats.polluted_considered,
            stats.reimported,
            stats.missing_file,
            stats.failed,
            stats.chunks_reimported,
            stats.polluted_remaining,
            stats.batch_secs,
            wall,
            stats.done
        );
        if stats.already_applied || stats.done {
            break;
        }
    }

    if total_secs > 0.001 {
        let throughput = total_chunks as f64 / total_secs;
        println!(
            "\nthroughput: {total_chunks} chunks / {total_secs:.2}s = {throughput:.1} chunks/sec"
        );
        let projected_280k = 280_000.0 / throughput;
        println!(
            "projected wall time for ~280,000 chunks at this rate: {:.1}s ({:.1} min)",
            projected_280k,
            projected_280k / 60.0
        );
    }

    println!("\n--- restamp ---");
    let restamp_stats = restamp_codex_chunk_timestamps(engine.storage())?;
    println!("{:#?}", restamp_stats);

    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let after_collapsed = collapsed_timestamp_conversations(&conn)?;
    let after_previews = harness_preview_conversations(&conn)?;
    println!("\n--- after ---");
    println!("collapsed-timestamp codex conversations: {after_collapsed} (was {before_collapsed})");
    println!("harness-preview codex conversations:      {after_previews} (was {before_previews})");

    Ok(())
}
