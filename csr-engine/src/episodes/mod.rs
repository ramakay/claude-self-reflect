//! S6 episode tier: a deterministic, model-free builder that turns mirrored
//! transcripts into `episodes` and `session_cards`, exports the tier for
//! `embed sample`, and writes probe session truth.
//!
//! Batch CLI only. The watcher is not wired here; that is the S9 list.

pub mod card;
pub mod parse;
pub mod store;
pub mod truth;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{json, Value};

use parse::{classify_drop, compact_record, row_text, Drops, Splitter};
use store::Watermark;

const TAIL_BYTES: usize = 4096;

#[derive(Debug, Default, Serialize)]
pub struct BuildStats {
    pub files: u64,
    pub reparsed_from_zero: u64,
    pub episodes: i64,
    pub session_cards: i64,
    pub conversations: i64,
    pub drops: Drops,
    pub compact_records: i64,
    pub conv_id_match_rate: f64,
    pub secs: f64,
    pub episodes_written: u64,
    pub skipped_csr_agent_files: u64,
    pub skipped_conflicting_conversation_id: u64,
    pub skipped_old_files: u64,
}

struct Origin {
    rel: String,
    conversation_id: String,
    project: String,
    source: &'static str,
}

fn origin(rel: &str) -> Origin {
    let rel = rel.strip_suffix(".zst").unwrap_or(rel).to_string();
    let comps: Vec<&str> = rel.split('/').collect();
    let name = comps.last().copied().unwrap_or("");
    let sidechain = comps.len() >= 4
        && comps[2] == "subagents"
        && name.starts_with("agent-")
        && name.ends_with(".jsonl");
    let project = if comps.len() >= 2 {
        let p = crate::import::normalize_project_name(comps[0]);
        if p.is_empty() {
            "unknown".to_string()
        } else {
            p
        }
    } else {
        "unknown".to_string()
    };
    let conversation_id = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    Origin {
        rel,
        conversation_id,
        project,
        source: if sidechain {
            "sidechain"
        } else {
            "conversation"
        },
    }
}

struct PassOut {
    episodes: Vec<parse::Episode>,
    compact: Vec<(String, &'static str, String)>,
    drops: Drops,
    offset: u64,
    tail_hash: String,
    resume_offset: u64,
    resume_ordinal: i64,
    agent_skipped: bool,
}

enum Pass {
    Done(Box<PassOut>),
    NeedFull,
}

fn open_reader(path: &Path) -> Result<Box<dyn BufRead>> {
    if path.extension().is_some_and(|e| e == "zst") {
        Ok(Box::new(crate::mirror::open_transcript(path)?))
    } else {
        Ok(Box::new(BufReader::with_capacity(
            1 << 16,
            std::fs::File::open(path)?,
        )))
    }
}

fn tail_hash(tail: &[u8]) -> String {
    let from = tail.len().saturating_sub(TAIL_BYTES);
    blake3::hash(&tail[from..]).to_hex().to_string()
}

fn run_pass(path: &Path, resume: Option<&Watermark>) -> Result<Pass> {
    let mut reader = open_reader(path).with_context(|| format!("open {}", path.display()))?;
    let start_off = resume.map_or(0, |w| w.resume_offset);
    let start_ord = resume.map_or(0, |w| w.resume_ordinal);
    let mut splitter = Splitter::new(start_ord);
    let mut sanitizer = crate::import::RowSanitizer::for_derived();
    let mut drops = Drops::default();
    let mut compact = Vec::new();
    let mut offset = 0u64;
    let mut tail: Vec<u8> = Vec::new();
    let mut verified = resume.is_none();
    let mut opening_checked = start_off > 0;
    let mut group: Option<(u64, i64)> = None;
    let mut agent_skipped = false;
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        let line_start = offset;
        let complete = buf.last() == Some(&b'\n');
        let parsed: Option<Value> = if line_start >= start_off {
            std::str::from_utf8(&buf)
                .ok()
                .and_then(|s| sonic_rs::from_str::<Value>(s.trim()).ok())
        } else {
            None
        };
        if !complete && (line_start < start_off || parsed.is_none()) {
            break; // a torn tail line is not consumed
        }
        offset += n as u64;
        tail.extend_from_slice(&buf);
        if tail.len() > 4 * TAIL_BYTES {
            let drop_to = tail.len() - TAIL_BYTES;
            tail.drain(..drop_to);
        }
        if let Some(w) = resume {
            if !verified && offset == w.source_bytes {
                if tail_hash(&tail) != w.tail_hash {
                    return Ok(Pass::NeedFull);
                }
                verified = true;
            } else if !verified && offset > w.source_bytes {
                return Ok(Pass::NeedFull);
            }
        }
        let Some(mut row) = parsed else { continue };
        if let Some((uuid, kind, ts)) = compact_record(&row) {
            compact.push((uuid, kind, ts));
        }
        if let Some(kind) = classify_drop(&row) {
            drops.count(kind);
            continue;
        }
        let ty = row.get("type").and_then(Value::as_str).unwrap_or("");
        if !matches!(ty, "user" | "human" | "assistant") {
            continue;
        }
        if !opening_checked && ty != "assistant" {
            let text = row_text(&row);
            if crate::import::is_csr_agent_prompt(&text) {
                agent_skipped = true;
                break;
            }
            if !text.trim().is_empty() && !parse::is_harness_prompt(&text) {
                opening_checked = true;
            }
        }
        sanitizer.sanitize(&mut row);
        if let Some(ord) = splitter.push(&row) {
            group = Some((line_start, ord));
        }
    }
    if resume.is_some() && !verified && !agent_skipped {
        return Ok(Pass::NeedFull);
    }
    if agent_skipped {
        return Ok(Pass::Done(Box::new(PassOut {
            episodes: Vec::new(),
            compact,
            drops,
            offset,
            tail_hash: tail_hash(&tail),
            resume_offset: offset,
            resume_ordinal: -1,
            agent_skipped: true,
        })));
    }
    let episodes = splitter.finish();
    let next_ordinal = episodes.last().map_or(start_ord, |e| e.ordinal + 1);
    let (resume_offset, resume_ordinal) = group.unwrap_or((offset, next_ordinal));
    Ok(Pass::Done(Box::new(PassOut {
        episodes,
        compact,
        drops,
        offset,
        tail_hash: tail_hash(&tail),
        resume_offset,
        resume_ordinal,
        agent_skipped: false,
    })))
}

fn collect_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if name.ends_with(".jsonl.zst") || name.ends_with(".jsonl") {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

fn cutoff_ts(days: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::days(days))
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string()
}

/// Build or refresh the episode tier from a mirror root.
pub fn build(conn: &Connection, root: &Path, since_days: Option<i64>) -> Result<BuildStats> {
    let started = Instant::now();
    let cutoff = since_days.map(cutoff_ts);
    let cutoff_time = since_days.map(|d| {
        std::time::SystemTime::now() - std::time::Duration::from_secs(d.max(0) as u64 * 86_400)
    });
    let mut stats = BuildStats::default();
    for path in collect_files(root) {
        if let Some(limit) = cutoff_time {
            let fresh = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .map(|m| m >= limit)
                .unwrap_or(true);
            if !fresh {
                stats.skipped_old_files += 1;
                continue;
            }
        }
        let rel_raw = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        let o = origin(&rel_raw);
        let wm = store::get_watermark(conn, &o.rel)?;
        if let Some(owner) = store::conversation_owner(conn, &o.conversation_id)? {
            if owner != o.rel {
                stats.skipped_conflicting_conversation_id += 1;
                continue;
            }
        }
        stats.files += 1;
        let (out, from_zero, delete_from) = match &wm {
            Some(w) if w.resume_ordinal < 0 => continue, // CSR agent run, nothing to extend
            Some(w) => match run_pass(&path, Some(w))? {
                Pass::Done(out) => (out, false, Some(w.resume_ordinal)),
                Pass::NeedFull => match run_pass(&path, None)? {
                    Pass::Done(out) => (out, true, None),
                    Pass::NeedFull => unreachable!("a full pass never asks for a full pass"),
                },
            },
            None => match run_pass(&path, None)? {
                Pass::Done(out) => (out, false, None),
                Pass::NeedFull => unreachable!("a full pass never asks for a full pass"),
            },
        };
        if from_zero {
            stats.reparsed_from_zero += 1;
        }
        if out.agent_skipped {
            stats.skipped_csr_agent_files += 1;
        }
        stats.drops.add(&out.drops);
        let tx = conn.unchecked_transaction()?;
        match delete_from {
            Some(ord) => store::delete_from_ordinal(&tx, &o.conversation_id, ord)?,
            None => store::delete_conversation(&tx, &o.conversation_id)?,
        }
        for e in &out.episodes {
            if cutoff
                .as_ref()
                .is_some_and(|c| e.timestamp.as_str() < c.as_str())
            {
                continue;
            }
            store::upsert_episode(&tx, &o.conversation_id, &o.project, o.source, e)?;
            stats.episodes_written += 1;
        }
        for (uuid, kind, ts) in &out.compact {
            store::insert_compact(&tx, uuid, &o.conversation_id, kind, ts)?;
        }
        card::rebuild_card(&tx, &o.conversation_id)?;
        store::set_watermark(
            &tx,
            &o.rel,
            &Watermark {
                conversation_id: o.conversation_id.clone(),
                source_bytes: out.offset,
                tail_hash: out.tail_hash.clone(),
                resume_offset: out.resume_offset,
                resume_ordinal: out.resume_ordinal,
            },
            &chrono::Utc::now().to_rfc3339(),
        )?;
        tx.commit()?;
    }
    stats.episodes = store::count(conn, "episodes")?;
    stats.session_cards = store::count(conn, "session_cards")?;
    stats.compact_records = store::count(conn, "episode_compact_records")?;
    stats.conversations = conn.query_row(
        "SELECT COUNT(DISTINCT conversation_id) FROM episodes",
        [],
        |r| r.get(0),
    )?;
    let matched: i64 = conn.query_row(
        "SELECT COUNT(*) FROM (SELECT DISTINCT conversation_id AS c FROM episodes)
         WHERE EXISTS (SELECT 1 FROM chunks WHERE conversation_id = c)",
        [],
        |r| r.get(0),
    )?;
    stats.conv_id_match_rate = if stats.conversations > 0 {
        matched as f64 / stats.conversations as f64
    } else {
        0.0
    };
    stats.secs = started.elapsed().as_secs_f64();
    Ok(stats)
}

/// Write the tier as `embed sample` rows. Returns the row count.
pub fn export(conn: &Connection, since_days: Option<i64>, out: &Path) -> Result<u64> {
    use std::io::Write;
    let cutoff = since_days.map(cutoff_ts).unwrap_or_default();
    let mut w = std::io::BufWriter::new(
        std::fs::File::create(out).with_context(|| format!("create {}", out.display()))?,
    );
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, timestamp, text FROM episodes
         WHERE timestamp >= ?1 ORDER BY rowid",
    )?;
    let mut rows = stmt.query([&cutoff])?;
    let mut n = 0u64;
    while let Some(r) = rows.next()? {
        let line = json!({
            "chunk_id": r.get::<_, String>(0)?,
            "conversation_id": r.get::<_, String>(1)?,
            "timestamp": r.get::<_, String>(2)?,
            "source": "episode",
            "origin": ["s6_tier"],
            "probes": [],
            "text": r.get::<_, String>(3)?,
        });
        writeln!(w, "{line}")?;
        n += 1;
    }
    w.flush()?;
    Ok(n)
}

pub fn cli_build(
    db: &Path,
    root: &Path,
    since_days: Option<i64>,
    stats_out: Option<&Path>,
) -> Result<String> {
    let storage = crate::storage::Storage::open(db)?;
    let stats = storage.with_connection(|conn| build(conn, root, since_days))?;
    let text = serde_json::to_string_pretty(&stats)?;
    if let Some(p) = stats_out {
        std::fs::write(p, format!("{text}\n"))?;
    }
    Ok(text)
}

pub fn cli_export(db: &Path, since_days: Option<i64>, out: &Path) -> Result<String> {
    let storage = crate::storage::Storage::open(db)?;
    let n = storage.with_connection(|conn| export(conn, since_days, out))?;
    Ok(format!("exported {n} episodes to {}", out.display()))
}

pub fn cli_truth(db: &Path, probes: &Path, out: &Path) -> Result<String> {
    let storage = crate::storage::Storage::open(db)?;
    storage.with_connection(|conn| truth::run(conn, probes, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::storage::migrations::run(&conn).unwrap();
        conn
    }

    fn user(uuid: &str, ts: &str, text: &str) -> String {
        json!({"type":"user","uuid":uuid,"timestamp":ts,"message":{"role":"user","content":text}})
            .to_string()
    }

    fn asst(uuid: &str, ts: &str, text: &str, tools: &[(&str, Value)]) -> String {
        let mut blocks = vec![json!({"type":"text","text":text})];
        for (name, input) in tools {
            blocks.push(json!({"type":"tool_use","id":"t1","name":name,"input":input}));
        }
        json!({"type":"assistant","uuid":uuid,"timestamp":ts,"message":{"role":"assistant","content":blocks}})
            .to_string()
    }

    fn write(root: &Path, rel: &str, lines: &[String]) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, lines.join("\n") + "\n").unwrap();
    }

    fn base_lines() -> Vec<String> {
        vec![
            user("u1", "2026-10-01T10:00:00Z", "fix the parser in src/foo/bar.rs"),
            asst(
                "a1",
                "2026-10-01T10:00:05Z",
                "We decided to use a streaming parser instead of a buffer. See commit abc1234def and PR #42.",
                &[
                    ("Read", json!({"file_path":"/src/foo/bar.rs"})),
                    ("TodoWrite", json!({"todos":[{"content":"write parser","status":"in_progress"}]})),
                ],
            ),
            user("u2", "2026-10-01T10:01:00Z", "now add a test"),
            asst("a2", "2026-10-01T10:01:05Z", "Added the test.", &[]),
        ]
    }

    fn dump(conn: &Connection) -> Vec<(String, i64, String, String)> {
        let mut s = conn
            .prepare("SELECT id, ordinal, text, first_uuid FROM episodes ORDER BY id")
            .unwrap();
        s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn build_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "-Users-x-projects-demo/sess-1.jsonl",
            &base_lines(),
        );
        let (a, b) = (db(), db());
        build(&a, dir.path(), None).unwrap();
        build(&b, dir.path(), None).unwrap();
        let (da, db_) = (dump(&a), dump(&b));
        assert_eq!(da.len(), 2);
        assert_eq!(da, db_);
        assert_eq!(da[0].0, "ep:sess-1:0");
        assert!(da[0].2.starts_with("U: fix the parser"));
        assert!(da[0].2.contains("[Read: /src/foo/bar.rs]"));
        // A second run on the same DB changes nothing.
        let again = build(&a, dir.path(), None).unwrap();
        assert_eq!(dump(&a), da);
        assert_eq!(again.reparsed_from_zero, 0);
        assert!(store::has_episodes(&a));
        assert_eq!(
            store::episode_session(&a, "ep:sess-1:1").as_deref(),
            Some("sess-1")
        );
    }

    #[test]
    fn truncated_transcript_reparses_from_zero() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "proj/sess-t.jsonl", &base_lines());
        let conn = db();
        build(&conn, dir.path(), None).unwrap();
        let wm = store::get_watermark(&conn, "proj/sess-t.jsonl")
            .unwrap()
            .unwrap();
        // Truncate below the watermark, then append different lines.
        let new_lines = [
            user("n1", "2026-10-02T09:00:00Z", "a different first prompt"),
            asst("n2", "2026-10-02T09:00:01Z", "reply", &[]),
        ];
        let short = base_lines()[0].clone();
        assert!((short.len() as u64) < wm.source_bytes);
        write(
            dir.path(),
            "proj/sess-t.jsonl",
            &[short, new_lines[0].clone(), new_lines[1].clone()],
        );
        let stats = build(&conn, dir.path(), None).unwrap();
        assert_eq!(stats.reparsed_from_zero, 1);

        let fresh_dir = tempfile::tempdir().unwrap();
        write(
            fresh_dir.path(),
            "proj/sess-t.jsonl",
            &[
                base_lines()[0].clone(),
                new_lines[0].clone(),
                new_lines[1].clone(),
            ],
        );
        let fresh = db();
        build(&fresh, fresh_dir.path(), None).unwrap();
        assert_eq!(dump(&conn), dump(&fresh));
        let ids: Vec<String> = dump(&conn).into_iter().map(|r| r.0).collect();
        let mut dedup = ids.clone();
        dedup.dedup();
        assert_eq!(ids, dedup, "no duplicate episodes");
    }

    #[test]
    fn append_resumes_from_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let mut lines = base_lines();
        write(dir.path(), "proj/sess-a.jsonl", &lines);
        let conn = db();
        build(&conn, dir.path(), None).unwrap();
        lines.push(asst("a3", "2026-10-01T10:02:00Z", "More on the test.", &[]));
        lines.push(user("u3", "2026-10-01T10:03:00Z", "ship it"));
        write(dir.path(), "proj/sess-a.jsonl", &lines);
        let stats = build(&conn, dir.path(), None).unwrap();
        assert_eq!(stats.reparsed_from_zero, 0);
        let fresh = db();
        build(&fresh, dir.path(), None).unwrap();
        assert_eq!(dump(&conn), dump(&fresh));
        assert_eq!(dump(&conn).len(), 3);
    }

    #[test]
    fn compact_records_keyed_by_uuid() {
        let dir = tempfile::tempdir().unwrap();
        let mut lines = base_lines();
        lines.push(json!({"type":"system","subtype":"compact_boundary","uuid":"cb1","timestamp":"2026-10-01T10:05:00Z"}).to_string());
        lines.push(json!({"type":"user","isCompactSummary":true,"uuid":"cs1","timestamp":"2026-10-01T10:05:01Z","message":{"role":"user","content":"summary of the session"}}).to_string());
        write(dir.path(), "proj/sess-c.jsonl", &lines);
        let conn = db();
        build(&conn, dir.path(), None).unwrap();
        assert_eq!(store::count(&conn, "episode_compact_records").unwrap(), 2);
        // Force a full re-parse: the record count stays 2.
        conn.execute("UPDATE episode_watermarks SET tail_hash = 'x'", [])
            .unwrap();
        let stats = build(&conn, dir.path(), None).unwrap();
        assert_eq!(stats.reparsed_from_zero, 1);
        assert_eq!(store::count(&conn, "episode_compact_records").unwrap(), 2);
        assert_eq!(stats.compact_records, 2);
    }

    #[test]
    fn drops_counted_by_type() {
        let dir = tempfile::tempdir().unwrap();
        let att = |t: &str, u: &str| {
            json!({"type":"attachment","uuid":u,"attachment":{"type":t,"content":"x"}}).to_string()
        };
        let lines = vec![
            att("hook_success", "d1"),
            att("hook_additional_context", "d2"),
            att("relevant_memories", "d3"),
            json!({"type":"system","subtype":"away_summary","uuid":"d4","content":"recap"}).to_string(),
            json!({"type":"user","isCompactSummary":true,"uuid":"d5","message":{"role":"user","content":"sum"}}).to_string(),
            json!({"type":"system","subtype":"api-request-failed","uuid":"d6"}).to_string(),
        ];
        write(dir.path(), "proj/sess-d.jsonl", &lines);
        let conn = db();
        let s = build(&conn, dir.path(), None).unwrap();
        let d = &s.drops;
        assert_eq!(
            (
                d.hook_success,
                d.hook_additional_context,
                d.relevant_memories,
                d.away_summary,
                d.is_compact_summary,
                d.api_request
            ),
            (1, 1, 1, 1, 1, 1)
        );
        assert_eq!(s.episodes, 0);
    }

    #[test]
    fn episode_cap_and_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let mut big = String::new();
        for i in 0..400 {
            if i == 200 {
                big.push_str("diff --git a/x b/x\n");
            }
            big.push_str(&format!(
                "line {i} of a very long assistant reply with some words\n"
            ));
        }
        let mut tools: Vec<(&str, Value)> = Vec::new();
        for i in 0..40 {
            tools.push(("Read", json!({"file_path": format!("/src/file_{i}.rs")})));
        }
        let lines = vec![
            user("u1", "2026-10-01T10:00:00Z", "explain everything"),
            asst("a1", "2026-10-01T10:00:05Z", &big, &tools),
        ];
        write(dir.path(), "proj/sess-cap.jsonl", &lines);
        let conn = db();
        let s = build(&conn, dir.path(), None).unwrap();
        assert!(s.episodes >= 3, "{}", s.episodes);
        let rows = dump(&conn);
        for (i, (_, ord, text, _)) in rows.iter().enumerate() {
            assert_eq!(*ord, i as i64, "ordinals are contiguous");
            assert!(text.chars().count() <= parse::EPISODE_CAP_CHARS, "over cap");
            for l in text.lines().filter(|l| l.starts_with("[Read")) {
                assert!(l.ends_with(']'), "tool line cut: {l}");
            }
        }
        let tool_lines: usize = rows
            .iter()
            .map(|r| r.2.lines().filter(|l| l.starts_with("[Read: ")).count())
            .sum();
        assert_eq!(tool_lines, 40, "no tool call lost or split");
        // Continuations repeat the prompt head.
        assert!(rows[1].2.starts_with("U: explain everything (cont.)"));
    }

    #[test]
    fn session_card_is_extractive() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "-Users-x-projects-demo/sess-k.jsonl",
            &base_lines(),
        );
        let conn = db();
        let s = build(&conn, dir.path(), None).unwrap();
        assert_eq!(s.session_cards, s.conversations);
        let (title, ids, decisions, task, receipts, count): (String, String, String, String, String, i64) = conn
            .query_row(
                "SELECT title, identifiers, decisions, task_state, receipts, episode_count FROM session_cards WHERE conversation_id='sess-k'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap();
        assert_eq!(title, "fix the parser in src/foo/bar.rs");
        assert!(title.chars().count() <= 120);
        assert!(ids.contains("bar.rs"), "{ids}");
        assert!(
            task.starts_with("TodoWrite: in_progress:write parser"),
            "{task}"
        );
        assert!(
            receipts.contains("commit:abc1234def") && receipts.contains("pr:42"),
            "{receipts}"
        );
        assert_eq!(count, 2);
        // Every extracted decision is a line of an episode: nothing generated.
        let all: String = dump(&conn)
            .into_iter()
            .map(|r| r.2)
            .collect::<Vec<_>>()
            .join("\n");
        let decisions: Vec<String> = serde_json::from_str(&decisions).unwrap();
        assert!(!decisions.is_empty());
        for d in decisions {
            assert!(all.contains(&d), "{d}");
        }
    }

    #[test]
    fn export_rows_match_sample_shape() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "proj/sess-e.jsonl", &base_lines());
        let conn = db();
        build(&conn, dir.path(), None).unwrap();
        let out = dir.path().join("out.jsonl");
        assert_eq!(export(&conn, None, &out).unwrap(), 2);
        let first: Value = serde_json::from_str(
            std::fs::read_to_string(&out)
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(first["chunk_id"], "ep:sess-e:0");
        assert_eq!(first["source"], "episode");
        assert_eq!(first["origin"], json!(["s6_tier"]));
        assert_eq!(first["probes"], json!([]));
        assert!(first["text"].as_str().unwrap().starts_with("U: "));
    }
}
