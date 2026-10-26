//! Probe session truth: which conversations hold an answer chunk for each probe.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use regex::{Regex, RegexBuilder, RegexSet, RegexSetBuilder};
use rusqlite::Connection;
use serde_json::{json, Value};

struct Compiled {
    must: RegexSet,
    must_not: Vec<Regex>,
    query: String,
    counts: BTreeMap<String, u64>,
}

fn patterns(probe: &Value, key: &str) -> Vec<String> {
    probe
        .get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Read `probes`, add `expect_sessions` and `expect_sessions_counts` per probe
/// from the `chunks` table, write `out`. Returns a one-line summary.
pub fn run(conn: &Connection, probes: &Path, out: &Path) -> Result<String> {
    let text =
        std::fs::read_to_string(probes).with_context(|| format!("read {}", probes.display()))?;
    let mut doc: Value = serde_json::from_str(&text).context("parse probes json")?;
    let list = doc
        .get("probes")
        .and_then(Value::as_array)
        .context("probes file has no probes array")?
        .clone();
    let mut compiled = Vec::new();
    for p in &list {
        let must = patterns(p, "must");
        if must.is_empty() {
            bail!(
                "probe {} has no must patterns",
                p.get("id").unwrap_or(&Value::Null)
            );
        }
        let must_set = RegexSetBuilder::new(&must)
            .case_insensitive(true)
            .build()
            .context("bad must regex")?;
        let must_not = patterns(p, "must_not")
            .iter()
            .map(|m| {
                RegexBuilder::new(m)
                    .case_insensitive(true)
                    .build()
                    .with_context(|| format!("bad must_not regex {m:?}"))
            })
            .collect::<Result<Vec<_>>>()?;
        compiled.push(Compiled {
            must: must_set,
            must_not,
            query: p
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            counts: BTreeMap::new(),
        });
    }
    let mut stmt = conn.prepare("SELECT conversation_id, content FROM chunks")?;
    let mut rows = stmt.query([])?;
    let mut scanned = 0u64;
    while let Some(row) = rows.next()? {
        scanned += 1;
        let conv: String = row.get(0)?;
        let content: String = row.get(1)?;
        for c in compiled.iter_mut() {
            if !c.must.is_match(&content) {
                continue;
            }
            if c.must_not.iter().any(|r| r.is_match(&content)) {
                continue;
            }
            if crate::search::echo::is_question_echo(&content, &c.query) {
                continue;
            }
            *c.counts.entry(conv.clone()).or_default() += 1;
        }
    }
    let mut summary = Vec::new();
    if let Some(arr) = doc.get_mut("probes").and_then(Value::as_array_mut) {
        for (p, c) in arr.iter_mut().zip(compiled.iter()) {
            let sessions: Vec<&String> = c.counts.keys().collect();
            summary.push(format!(
                "#{}:{}",
                p.get("id").map(Value::to_string).unwrap_or_default(),
                sessions.len()
            ));
            p["expect_sessions"] = json!(sessions);
            p["expect_sessions_counts"] = json!(c.counts);
        }
    }
    std::fs::write(out, serde_json::to_string_pretty(&doc)? + "\n")
        .with_context(|| format!("write {}", out.display()))?;
    Ok(format!(
        "truth: scanned {scanned} chunks; sessions per probe {}",
        summary.join(" ")
    ))
}
