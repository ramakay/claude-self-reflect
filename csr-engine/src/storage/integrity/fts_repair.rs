//! Remove keyword-index rows whose chunk is gone, without rebuilding the index.
//!
//! `chunks_fts` is external content: FTS5 keeps no copy of the text, and its
//! `delete` command must be given the exact text that was indexed, or it
//! removes the wrong terms. The text of a dangling rowid went with its chunk,
//! but the index still holds every token that text produced, at its offset
//! (`fts5vocab` `instance`). For each dangling rowid the repair builds a text
//! that the same tokenizer turns into exactly those tokens at exactly those
//! offsets, checks that in a scratch FTS5 table in `temp` (every instance and
//! the encoded docsize must match), and only then issues the `delete`. A rowid
//! whose text cannot be rebuilt exactly is left alone and counted.
//!
//! The tokenizer stems (porter), and porter is not a fixed point on its own
//! output: "because" is indexed as "becaus", and "becaus" stems again to
//! something else. So each indexed term needs a word that stems to it: the
//! term itself, or the term plus a common suffix, each probed through the
//! same tokenizer. On the standard corpus copy (172 dangling rowids, 2,756
//! distinct terms) the bare term worked for 2,700 terms and a suffix for the
//! other 56. Over that copy's whole vocabulary (779,117 terms) the list below
//! finds a word for every term; "l" is for terms ending "ll" ("farewellL"
//! folds to "farewelll", which stems to "farewell").

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

/// Most dangling rowids one pass repairs, so one pass stays bounded. A later
/// pass takes the next ones.
pub(crate) const MAX_ROWIDS_PER_PASS: usize = 5_000;

/// Suffixes tried, in order, to find a word the tokenizer stems to a term.
const SUFFIXES: &[&str] = &[
    "", "e", "s", "ed", "ing", "es", "ion", "al", "ly", "y", "er", "ate", "ance", "ence", "ive",
    "ize", "ment", "ness", "ful", "ous", "ity", "able", "ible", "ism", "ist", "ent", "ant", "l",
];

const SCRATCH: &str = "
    DROP TABLE IF EXISTS temp.csr_fts_repair_probe_vocab;
    DROP TABLE IF EXISTS temp.csr_fts_repair_probe;
    DROP TABLE IF EXISTS temp.csr_fts_repair_vocab;
    DROP TABLE IF EXISTS temp.csr_fts_repair_ids;
";

/// What one repair pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FtsRepair {
    /// Dangling rowids the pass looked at (at most [`MAX_ROWIDS_PER_PASS`]).
    pub found: u64,
    /// Removed from the index with a verified `delete`.
    pub removed: u64,
    /// Left in place: no text was found that the tokenizer turns into the
    /// indexed tokens, so a `delete` could not be issued safely.
    pub unresolved: u64,
    /// The whole index was rebuilt from `chunks` (only when the caller allows
    /// it and the targeted pass could not remove everything).
    pub rebuilt: bool,
}

/// The `tokenize=` literal of the `chunks_fts` declaration, exactly as
/// written (quotes included); `None` when the declaration names no tokenizer.
fn tokenize_literal(schema: &str) -> Option<&str> {
    let lower = schema.to_ascii_lowercase();
    let start = lower.find("tokenize")? + "tokenize".len();
    let rest = schema[start..].trim_start().strip_prefix('=')?.trim_start();
    let offset = schema.len() - rest.len();
    let quote = rest.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let bytes = rest.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        if bytes[i] == quote as u8 {
            if bytes.get(i + 1) == Some(&(quote as u8)) {
                i += 2;
                continue;
            }
            return Some(&schema[offset..offset + i + 1]);
        }
        i += 1;
    }
    None
}

/// Tokens per rowid, in offset order, read from a `fts5vocab` instance table.
/// `None` for a rowid whose instances are not one column at offsets 0..n.
fn read_instances(
    conn: &Connection,
    sql: &str,
    column: &str,
) -> Result<BTreeMap<i64, Option<Vec<String>>>> {
    let mut by_doc: BTreeMap<i64, Vec<(i64, String, String)>> = BTreeMap::new();
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    for row in rows {
        let (doc, col, term, offset) = row?;
        by_doc.entry(doc).or_default().push((offset, col, term));
    }
    Ok(by_doc
        .into_iter()
        .map(|(doc, mut tokens)| {
            tokens.sort_by_key(|(offset, _, _)| *offset);
            let exact = tokens
                .iter()
                .enumerate()
                .all(|(i, (offset, col, _))| *offset == i as i64 && col == column);
            let terms = exact.then(|| tokens.into_iter().map(|(_, _, term)| term).collect());
            (doc, terms)
        })
        .collect())
}

/// Read the probe table's tokens per rowid.
fn probe_tokens(conn: &Connection) -> Result<BTreeMap<i64, Option<Vec<String>>>> {
    read_instances(
        conn,
        "SELECT doc, col, term, offset FROM temp.csr_fts_repair_probe_vocab",
        "content",
    )
}

/// A word per term that the tokenizer turns into exactly that one term.
fn preimages(conn: &Connection, terms: &BTreeSet<String>) -> Result<HashMap<String, String>> {
    let mut found = HashMap::new();
    let mut pending: Vec<&String> = terms.iter().collect();
    for suffix in SUFFIXES {
        if pending.is_empty() {
            break;
        }
        conn.execute("DELETE FROM temp.csr_fts_repair_probe", [])?;
        {
            let mut insert = conn
                .prepare("INSERT INTO temp.csr_fts_repair_probe(rowid, content) VALUES (?1, ?2)")?;
            for (i, term) in pending.iter().enumerate() {
                insert.execute(params![i as i64 + 1, format!("{term}{suffix}")])?;
            }
        }
        let tokens = probe_tokens(conn)?;
        let mut still = Vec::new();
        for (i, term) in pending.into_iter().enumerate() {
            let one = tokens
                .get(&(i as i64 + 1))
                .and_then(Option::as_ref)
                .is_some_and(|t| t.len() == 1 && t[0] == **term);
            if one {
                found.insert(term.clone(), format!("{term}{suffix}"));
            } else {
                still.push(term);
            }
        }
        pending = still;
    }
    Ok(found)
}

/// Remove dangling `chunks_fts` rowids (indexed rowids with no chunk). With
/// `allow_rebuild`, rowids the targeted pass cannot remove (or more than one
/// pass takes) are cleared by rebuilding the whole index from `chunks`
/// instead: about 46 s on a 1M-chunk store, so only a foreground caller
/// passes it. Returns a zero report when the store has no keyword index.
pub(crate) fn repair_dangling(conn: &Connection, allow_rebuild: bool) -> Result<FtsRepair> {
    repair_dangling_capped(conn, allow_rebuild, MAX_ROWIDS_PER_PASS)
}

fn repair_dangling_capped(conn: &Connection, allow_rebuild: bool, cap: usize) -> Result<FtsRepair> {
    let Some(schema) = chunks_fts_schema(conn)? else {
        return Ok(FtsRepair::default());
    };
    let dangling: Vec<i64> = {
        let mut stmt = conn.prepare(
            "SELECT d.id FROM chunks_fts_docsize d
             WHERE NOT EXISTS (SELECT 1 FROM chunks c WHERE c.rowid = d.id)
             ORDER BY d.id LIMIT ?1",
        )?;
        let ids = stmt.query_map([cap as i64 + 1], |row| row.get(0))?;
        ids.collect::<rusqlite::Result<_>>()?
    };
    if dangling.is_empty() {
        return Ok(FtsRepair::default());
    }
    let over_cap = dangling.len() > cap;
    let dangling = &dangling[..dangling.len().min(cap)];

    let prepared = prepare(conn, &schema, dangling);
    let _ = conn.execute_batch(SCRATCH);
    let found = dangling.len() as u64;
    let removed = delete_verified(conn, &prepared?)?;
    let mut report = FtsRepair {
        found,
        removed,
        unresolved: found - removed,
        rebuilt: false,
    };
    if allow_rebuild && (report.unresolved > 0 || over_cap) {
        rebuild(conn)?;
        report.rebuilt = true;
    }
    Ok(report)
}

fn chunks_fts_schema(conn: &Connection) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'chunks_fts'",
            [],
            |row| row.get(0),
        )
        .optional()?)
}

/// A dangling rowid with a text the tokenizer turns into exactly what the
/// index holds for it, and the docsize row that text was checked against.
#[derive(Debug, Clone)]
struct Verified {
    rowid: i64,
    text: String,
    docsize: Vec<u8>,
}

/// Rowids deleted per savepoint, at most.
const BATCH_ROWS: usize = 500;
/// A batch stops early once it has held the write lock this long.
const BATCH_BUDGET: std::time::Duration = std::time::Duration::from_millis(250);

/// Issue the verified deletes in short savepoints (at most [`BATCH_ROWS`]
/// rowids or [`BATCH_BUDGET`] each), so other writers wait for one batch at
/// most. Each rowid is re-read inside its batch and deleted only while no
/// chunk owns it and its docsize row is byte-equal to the one its text was
/// verified against: anything that touched the rowid since then (a repair
/// elsewhere, a rebuild, a different text stranded there) leaves it alone.
/// Returns the rowids deleted.
fn delete_verified(conn: &Connection, verified: &[Verified]) -> Result<u64> {
    let mut current = conn.prepare(
        "SELECT sz FROM chunks_fts_docsize
         WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM chunks WHERE rowid = ?1)",
    )?;
    let mut delete = conn
        .prepare("INSERT INTO chunks_fts(chunks_fts, rowid, content) VALUES ('delete', ?1, ?2)")?;
    let mut removed = 0u64;
    let mut next = 0usize;
    while next < verified.len() {
        conn.execute_batch("SAVEPOINT csr_fts_repair")?;
        let started = std::time::Instant::now();
        let end = (next + BATCH_ROWS).min(verified.len());
        let batch = (|| -> Result<u64> {
            let mut deleted = 0u64;
            while next < end {
                let row = &verified[next];
                next += 1;
                let docsize: Option<Vec<u8>> =
                    current.query_row([row.rowid], |r| r.get(0)).optional()?;
                if docsize.as_deref() == Some(row.docsize.as_slice()) {
                    delete.execute(params![row.rowid, row.text])?;
                    deleted += 1;
                }
                if started.elapsed() >= BATCH_BUDGET {
                    break;
                }
            }
            Ok(deleted)
        })();
        match batch {
            Ok(deleted) => {
                conn.execute_batch("RELEASE csr_fts_repair")?;
                removed += deleted;
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK TO csr_fts_repair; RELEASE csr_fts_repair");
                return Err(error);
            }
        }
    }
    Ok(removed)
}

/// Read what the index holds for each dangling rowid and build a verified
/// text for each one it can. The index tokens and the docsize rows are read
/// in one read transaction, so both come from the same snapshot (the scratch
/// tables live in `temp` and take no lock on the store). The caller drops
/// the scratch tables ([`SCRATCH`]).
fn prepare(conn: &Connection, schema: &str, dangling: &[i64]) -> Result<Vec<Verified>> {
    conn.execute_batch("SAVEPOINT csr_fts_repair_read")?;
    let prepared = prepare_in_snapshot(conn, schema, dangling);
    // Nothing in the store was written: rolling back only ends the snapshot
    // (and undoes the scratch rows, which are no longer needed).
    let _ = conn.execute_batch("ROLLBACK TO csr_fts_repair_read; RELEASE csr_fts_repair_read");
    prepared
}

fn prepare_in_snapshot(conn: &Connection, schema: &str, dangling: &[i64]) -> Result<Vec<Verified>> {
    let tokenize = tokenize_literal(schema)
        .map(|literal| format!(", tokenize={literal}"))
        .unwrap_or_default();
    conn.execute_batch(SCRATCH)?;
    conn.execute_batch(&format!(
        "CREATE TEMP TABLE csr_fts_repair_ids(id INTEGER PRIMARY KEY);
         CREATE VIRTUAL TABLE temp.csr_fts_repair_vocab USING fts5vocab(main, chunks_fts, instance);
         CREATE VIRTUAL TABLE temp.csr_fts_repair_probe USING fts5(content{tokenize});
         CREATE VIRTUAL TABLE temp.csr_fts_repair_probe_vocab
             USING fts5vocab(temp, csr_fts_repair_probe, instance);"
    ))?;
    {
        let mut insert = conn.prepare("INSERT INTO temp.csr_fts_repair_ids(id) VALUES (?1)")?;
        for id in dangling {
            insert.execute([id])?;
        }
    }

    // What the index holds for each dangling rowid. A rowid with a docsize
    // row and no instances indexed an empty text.
    let mut indexed = read_instances(
        conn,
        "SELECT doc, col, term, offset FROM temp.csr_fts_repair_vocab
         WHERE doc IN (SELECT id FROM temp.csr_fts_repair_ids)",
        "content",
    )?;
    for id in dangling {
        indexed.entry(*id).or_insert_with(|| Some(Vec::new()));
    }

    let terms: BTreeSet<String> = indexed.values().flatten().flatten().cloned().collect();
    let words = preimages(conn, &terms)?;
    let mut candidates: BTreeMap<i64, String> = BTreeMap::new();
    for (doc, tokens) in &indexed {
        let Some(tokens) = tokens else { continue };
        let text: Option<Vec<&str>> = tokens
            .iter()
            .map(|term| words.get(term).map(String::as_str))
            .collect();
        if let Some(text) = text {
            candidates.insert(*doc, text.join(" "));
        }
    }

    // Verify each candidate through the same tokenizer: identical tokens at
    // identical offsets, and an identical encoded docsize.
    conn.execute("DELETE FROM temp.csr_fts_repair_probe", [])?;
    {
        let mut insert =
            conn.prepare("INSERT INTO temp.csr_fts_repair_probe(rowid, content) VALUES (?1, ?2)")?;
        for (doc, text) in &candidates {
            insert.execute(params![doc, text])?;
        }
    }
    let probed = probe_tokens(conn)?;
    let mut verified = Vec::new();
    for (doc, text) in candidates {
        let want = indexed.get(&doc).cloned().flatten().unwrap_or_default();
        let got = probed.get(&doc).cloned().flatten().unwrap_or_default();
        let sizes: (Option<Vec<u8>>, Option<Vec<u8>>) = conn.query_row(
            "SELECT (SELECT sz FROM main.chunks_fts_docsize WHERE id = ?1),
                    (SELECT sz FROM temp.csr_fts_repair_probe_docsize WHERE id = ?1)",
            [doc],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let (Some(docsize), probe) = sizes else {
            continue;
        };
        if want == got && probe.as_ref() == Some(&docsize) {
            verified.push(Verified {
                rowid: doc,
                text,
                docsize,
            });
        }
    }
    Ok(verified)
}

fn rebuild(conn: &Connection) -> Result<()> {
    conn.execute_batch("SAVEPOINT csr_fts_repair_rebuild")?;
    match conn.execute("INSERT INTO chunks_fts(chunks_fts) VALUES ('rebuild')", []) {
        Ok(_) => conn.execute_batch("RELEASE csr_fts_repair_rebuild")?,
        Err(error) => {
            let _ = conn.execute_batch(
                "ROLLBACK TO csr_fts_repair_rebuild; RELEASE csr_fts_repair_rebuild",
            );
            return Err(error.into());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "fts_repair_tests.rs"]
mod tests;
