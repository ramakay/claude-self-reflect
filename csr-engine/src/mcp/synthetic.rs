//! Synthetic and eval content, judged by provenance.
//!
//! The rewrite's own audit and bench runs leave transcripts in the corpus:
//! sessions that drive `csr-engine bench`, `probe_ab.py` or `measure.py` from a
//! `.testbed/` working tree, and the tool output those runs print (probe
//! answers, scorecard rows, planted E3 answers). Recall must not answer a
//! question with the rig that measured it.
//!
//! The verdict never reads the candidate row's own text. It reads the session
//! the row belongs to, through two signals from the *other* chunks:
//!
//! 1. **Eval session.** A large enough share of the session's chunks carry an
//!    eval marker (a `.testbed/` path, a `bench --probe` line, scorecard keys),
//!    or most of the session's Bash calls run bench/eval/probe/measure tools.
//!    The session's own prose (assistant and user chunks) leaves the pool. Its
//!    tool output stays: memory files are not indexed, so an eval worker's
//!    file reads are the only copy of that source text.
//! 2. **Eval call output.** In any other session, a tool-result chunk that
//!    directly follows an assistant chunk holding such a Bash call is that
//!    call's output and leaves the pool.
//! 3. **CSR output.** A run of three or more consecutive tool-result chunks
//!    in which at least one chunk in twelve, or five chunks of any run, print
//!    CSR's own recall output (`csr_reflect_on_past`, a results header) is a
//!    printed tool response, not content, and leaves the pool.
//!
//! The verdict is read once per session per process (with a short TTL so a
//! live session is re-read) and cached, so the warm path costs a map lookup.
//! `CSR_NO_SYNTHETIC_FILTER=1` turns the filter off.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::provenance::Speaker;
use crate::storage::queries::EvalScanRow;
use crate::storage::Storage;

/// A session verdict is re-read after this long, so a session that is still
/// being written is not judged on a stale prefix forever.
const VERDICT_TTL: Duration = Duration::from_secs(600);
/// Cache size bound; the map is cleared when it grows past this.
const CACHE_CAP: usize = 20_000;
/// Marked chunks a session needs before the share rule applies.
const MIN_MARKED_CHUNKS: usize = 5;
/// Share rule: marked chunks times this must reach the session's chunk count
/// (so one marked chunk in ten).
const MARKED_SHARE_DENOM: usize = 10;
/// Call rule: eval Bash calls a session needs, and the share of its Bash calls
/// they must make up (one in four).
const MIN_EVAL_CALLS: usize = 2;
const EVAL_CALL_SHARE_DENOM: usize = 4;
/// A tool-result run needs this many chunks before the CSR-output rule reads it.
const MIN_OUTPUT_RUN: usize = 3;
/// CSR-output rule: marked chunks times this must reach the run length (one in
/// twelve), or the run holds this many marked chunks outright.
const OUTPUT_RUN_SHARE_DENOM: usize = 12;
const OUTPUT_RUN_MIN_PRINTED: usize = 5;
/// A tool-result chunk this many places after an eval call chunk, or fewer,
/// is taken as that call's output.
const OUTPUT_REACH: i64 = 2;

/// SQL LIKE patterns that mark a chunk as eval rig material. `_` is a
/// single-character wildcard in LIKE; the looseness is harmless here.
pub(crate) const MARKER_LIKE: &[&str] = &[
    "%.testbed/%",
    "%bench --probe%",
    "%bench --db-path%",
    "%probe_ab%",
    "%rank1_id%",
    "%echo_at_rank1%",
];

/// SQL LIKE patterns for chunks that print CSR's own recall output; the
/// markers match the echo definition of the study's classifier.
pub(crate) const OUTPUT_LIKE: &[&str] = &[
    "%🎯 RESULTS%",
    "%<summary count=%",
    "%csr_reflect_on_past%",
    "%mcp__claude-self-reflect__%",
    "%</caveat>%",
];

/// `CSR_NO_SYNTHETIC_FILTER=1` keeps eval and synthetic content in recall.
pub(crate) fn filter_enabled() -> bool {
    std::env::var("CSR_NO_SYNTHETIC_FILTER").map_or(true, |value| value != "1")
}

/// One candidate chunk, as the filter needs to see it.
#[derive(Debug, Clone)]
pub(crate) struct ChunkRef {
    pub id: String,
    pub conversation_id: String,
    /// Stored author; `None` when the chunk has no provenance row.
    pub author: Option<Speaker>,
}

/// What one scan says about a session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SessionVerdict {
    /// The session's own prose is eval material.
    pub eval_session: bool,
    /// Tool-result chunks that are printed tool output rather than content:
    /// the output of a bench/eval/probe/measure call (normal sessions only)
    /// and runs that print CSR's own recall output.
    pub output_ids: HashSet<String>,
    /// The session ran the rig (eval Bash calls reach [`MIN_EVAL_CALLS`]) even
    /// when those calls are a small share of a long working session.
    pub ran_rig: bool,
    /// Chunks marked as rig material (a rig marker or a printed CSR response).
    pub rig_ids: HashSet<String>,
    /// The session that spawned this one, when its chunks name another.
    pub parent: Option<String>,
}

fn call_pattern() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"csr-engine\S*\s+(?:bench|eval)\b|probe_ab\.py|measure\.py\s+\S*\.ndjson|(?:\bcd|\s-C)\s+\S*/\.testbed/",
        )
        .expect("static pattern")
    })
}

fn bash_marker() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"\[Bash: |external_agent_tool_call: Bash\]").expect("static pattern")
    })
}

/// Bash calls in one assistant chunk: `(total, eval)`. A call's text is the 600
/// characters after its marker; the importer stores calls truncated.
fn count_calls(text: &str) -> (usize, usize) {
    let mut total = 0;
    let mut eval = 0;
    for marker in bash_marker().find_iter(text) {
        total += 1;
        let mut end = (marker.start() + 600).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if call_pattern().is_match(&text[marker.start()..end]) {
            eval += 1;
        }
    }
    (total, eval)
}

/// The verdict for a session from its scan rows. Pure.
pub(crate) fn judge_session(rows: &[EvalScanRow]) -> SessionVerdict {
    let mut marked = 0usize;
    let mut calls = 0usize;
    let mut eval_calls = 0usize;
    let mut call_seqs = Vec::new();
    for row in rows {
        let mut chunk_marked = row.marked;
        if let Some(text) = row.bash_text.as_deref() {
            let (total, eval) = count_calls(text);
            calls += total;
            eval_calls += eval;
            if eval > 0 {
                call_seqs.push(row.seq);
                chunk_marked = true;
            }
        }
        if chunk_marked {
            marked += 1;
        }
    }
    let share_rule = marked >= MIN_MARKED_CHUNKS && marked * MARKED_SHARE_DENOM >= rows.len();
    let call_rule = eval_calls >= MIN_EVAL_CALLS && eval_calls * EVAL_CALL_SHARE_DENOM >= calls;
    let eval_session = share_rule || call_rule;
    let ran_rig = eval_calls >= MIN_EVAL_CALLS;
    let rig_ids: HashSet<String> = rows
        .iter()
        .filter(|row| row.marked || row.output_marked)
        .map(|row| row.id.clone())
        .collect();
    let mut output_ids = printed_output_ids(rows);
    if !eval_session {
        for row in rows {
            if row.author.as_deref() == Some("tool_result")
                && call_seqs
                    .iter()
                    .any(|&call| row.seq > call && row.seq <= call + OUTPUT_REACH)
            {
                output_ids.insert(row.id.clone());
            }
        }
    }
    SessionVerdict {
        eval_session,
        output_ids,
        ran_rig,
        rig_ids,
        parent: None,
    }
}

/// The session that spawned `conversation`: the other conversation its chunks
/// were sourced from. `None` for a session that is its own source.
fn parent_of(rows: &[EvalScanRow], conversation: &str) -> Option<String> {
    rows.iter()
        .filter_map(|row| row.source_conv.as_deref())
        .find(|source| *source != conversation)
        .map(str::to_string)
}

/// Ids of tool-result runs (consecutive chunk numbers) that print CSR
/// output. Pure.
fn printed_output_ids(rows: &[EvalScanRow]) -> HashSet<String> {
    let mut results: Vec<&EvalScanRow> = rows
        .iter()
        .filter(|row| row.author.as_deref() == Some("tool_result"))
        .collect();
    results.sort_by_key(|row| row.seq);
    let mut ids = HashSet::new();
    let mut start = 0;
    while start < results.len() {
        let mut end = start;
        while end + 1 < results.len() && results[end + 1].seq == results[end].seq + 1 {
            end += 1;
        }
        let run = &results[start..=end];
        let printed = run.iter().filter(|row| row.output_marked).count();
        if run.len() >= MIN_OUTPUT_RUN
            && printed > 0
            && (printed * OUTPUT_RUN_SHARE_DENOM >= run.len() || printed >= OUTPUT_RUN_MIN_PRINTED)
        {
            ids.extend(run.iter().map(|row| row.id.clone()));
        }
        start = end + 1;
    }
    ids
}

/// True when `chunk` is dropped under `verdict`.
pub(crate) fn dropped_by(verdict: &SessionVerdict, chunk: &ChunkRef) -> bool {
    if chunk.author == Some(Speaker::ToolResult) {
        verdict.output_ids.contains(&chunk.id)
    } else {
        verdict.eval_session
    }
}

type Cache = Mutex<HashMap<String, (Instant, Arc<SessionVerdict>)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Ids among `candidates` that are synthetic or eval content. Empty when the
/// kill switch is set. Stored reflections must not be passed in: they are
/// written on purpose and judged elsewhere.
pub(crate) fn synthetic_ids(storage: &Storage, candidates: &[ChunkRef]) -> HashSet<String> {
    synthetic_ids_if(filter_enabled(), storage, candidates)
}

fn synthetic_ids_if(enabled: bool, storage: &Storage, candidates: &[ChunkRef]) -> HashSet<String> {
    let mut dropped = HashSet::new();
    if !enabled {
        return dropped;
    }
    // Distinct conversations, first-seen order. The ones the process cache
    // cannot answer are read in one query, not one query each.
    let mut seen: HashSet<&str> = HashSet::new();
    let conversations: Vec<&str> = candidates
        .iter()
        .map(|chunk| chunk.conversation_id.as_str())
        .filter(|conversation| seen.insert(*conversation))
        .collect();
    crate::mcp::stage_timing::count("synthetic_conversations", conversations.len() as u64);
    let verdicts = session_verdicts(storage, &conversations);
    for chunk in candidates {
        if let Some(verdict) = verdicts.get(chunk.conversation_id.as_str()) {
            if dropped_by(verdict, chunk) {
                dropped.insert(chunk.id.clone());
            }
        }
    }
    dropped
}

/// Judge `conversations` into the process cache ahead of the filter that will
/// ask for them. Nothing is returned and nothing is decided here.
pub(crate) fn prefetch_verdicts(storage: &Storage, conversations: &[&str]) {
    let _ = session_verdicts(storage, conversations);
}

/// The verdict of each of `conversations`: from the process cache while it is
/// fresh, the rest from one batched scan (cached on the way out).
fn session_verdicts<'a>(
    storage: &Storage,
    conversations: &[&'a str],
) -> HashMap<&'a str, Arc<SessionVerdict>> {
    let mut verdicts: HashMap<&'a str, Arc<SessionVerdict>> = HashMap::new();
    let mut uncached: Vec<String> = Vec::new();
    {
        let held = cache().lock().unwrap_or_else(|e| e.into_inner());
        for &conversation in conversations {
            match held.get(conversation) {
                Some((at, verdict)) if at.elapsed() < VERDICT_TTL => {
                    verdicts.insert(conversation, Arc::clone(verdict));
                }
                _ => uncached.push(conversation.to_string()),
            }
        }
    }
    crate::mcp::stage_timing::count(
        "synthetic_cache_hits",
        (conversations.len() - uncached.len()) as u64,
    );
    if uncached.is_empty() {
        return verdicts;
    }
    // A failed read is a pass for this call (the filter never removes what it
    // cannot judge) but never a verdict: it is not cached, so the next ask
    // scans again instead of keeping "clean" for the whole TTL.
    let scan_start = Instant::now();
    let scan = scan_conversations(storage, &uncached);
    if crate::mcp::stage_timing::enabled() {
        crate::mcp::stage_timing::add_ms(
            "synthetic_scan",
            scan_start.elapsed().as_secs_f64() * 1000.0,
        );
    }
    let scan_ok = scan.is_ok();
    let mut scanned = scan.unwrap_or_default();
    let mut held = cache().lock().unwrap_or_else(|e| e.into_inner());
    if scan_ok && held.len() + uncached.len() >= CACHE_CAP {
        held.clear();
    }
    for &conversation in conversations {
        if verdicts.contains_key(conversation) {
            continue;
        }
        let rows = scanned.remove(conversation).unwrap_or_default();
        let mut judged = judge_session(&rows);
        judged.parent = parent_of(&rows, conversation);
        let verdict = Arc::new(judged);
        if scan_ok {
            held.insert(
                conversation.to_string(),
                (Instant::now(), Arc::clone(&verdict)),
            );
        }
        verdicts.insert(conversation, verdict);
    }
    verdicts
}

/// Conversations a scan must hold before it is split across connections: one
/// batch of a few conversations is cheaper read in place than set up in threads.
const PARALLEL_SCAN_FROM: usize = 6;
/// Connections (threads) a large scan is split across.
const SCAN_LANES: usize = 4;

type ScanRows = HashMap<String, Vec<EvalScanRow>>;

/// The scan rows of `conversations`, keyed by conversation. A large set is
/// dealt round-robin over [`SCAN_LANES`] read-only connections read at once
/// (every conversation is scanned by exactly one of them, so the rows are the
/// ones a single read would return); a small one, an in-memory store, or a
/// failure to open the other connections reads on `storage` alone. Any failure
/// of a lane (open, query or panic) discards the lanes' rows and reads the
/// whole set serially: a partial scan would leave sessions looking clean.
/// `CSR_NO_RECALL_LANES=1` keeps the single read.
fn scan_conversations(storage: &Storage, conversations: &[String]) -> anyhow::Result<ScanRows> {
    scan_conversations_with(storage, conversations, |lane_storage, group| {
        let Some(lane) = lane_storage.open_read_lane()? else {
            return Ok(None);
        };
        lane.with_connection(|conn| {
            crate::storage::queries::conversation_eval_scan_many(
                conn,
                group,
                MARKER_LIKE,
                OUTPUT_LIKE,
            )
        })
        .map(Some)
    })
}

/// [`scan_conversations`] with the read of one lane given (`Ok(None)`: no
/// second connection to be had), so a test can make a lane fail.
fn scan_conversations_with<F>(
    storage: &Storage,
    conversations: &[String],
    lane_scan: F,
) -> anyhow::Result<ScanRows>
where
    F: Fn(&Storage, &[String]) -> anyhow::Result<Option<ScanRows>> + Sync,
{
    let single = |ids: &[String]| {
        storage.with_connection(|conn| {
            crate::storage::queries::conversation_eval_scan_many(
                conn,
                ids,
                MARKER_LIKE,
                OUTPUT_LIKE,
            )
        })
    };
    if conversations.len() < PARALLEL_SCAN_FROM || !super::tools::recall_lanes_enabled() {
        return single(conversations);
    }
    let lanes = SCAN_LANES;
    let mut groups: Vec<Vec<String>> = vec![Vec::new(); lanes];
    for (n, conversation) in conversations.iter().enumerate() {
        groups[n % lanes].push(conversation.clone());
    }
    let lane_scan = &lane_scan;
    let parts: Vec<anyhow::Result<Option<ScanRows>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = groups[1..]
            .iter()
            .map(|group| scope.spawn(move || lane_scan(storage, group)))
            .collect();
        let mut out = vec![single(&groups[0]).map(Some)];
        out.extend(handles.into_iter().map(|handle| {
            handle
                .join()
                .unwrap_or_else(|_| Err(anyhow::anyhow!("scan thread panicked")))
        }));
        out
    });
    let mut found: ScanRows = HashMap::new();
    for part in parts {
        match part {
            Ok(Some(rows)) => found.extend(rows),
            // No second connection (an in-memory store) or a failed lane: the
            // whole set is read again, serially, on `storage`.
            Ok(None) | Err(_) => return single(conversations),
        }
    }
    Ok(found)
}

/// One candidate row for [`rig_sunk_ids`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct SinkRef<'a> {
    pub id: &'a str,
    pub conversation_id: &'a str,
    pub content: &'a str,
}

/// Ids among `candidates` that rank below every other row, because the session
/// they belong to ran the eval rig and they are the rig's own material. Demote
/// only: the caller keeps them, so they still answer when nothing else does.
///
/// A session is read once (cached with [`session_verdicts`]); a sub-agent also
/// takes the standing of the session that spawned it, one level up. Two tiers:
///
/// - **Eval session** (the share or call rule of [`judge_session`]), or a
///   sub-agent of one: a row marked as rig material (a `.testbed/` path, a
///   `bench --probe` line, a printed CSR response) sinks. Its prose already left
///   the pool; its file reads are not marked and stay.
/// - **Ran the rig** (two or more eval calls, however small a share), or a
///   sub-agent of such a session: a row that quotes the whole `query` sinks. In
///   a session that measured recall, a row holding the audit question is the
///   rig's copy of it (a probe prompt, a query list, an interview).
///
/// The text of the candidate is read only for the second tier, and only after
/// the session earned it from its other chunks. The same text in a session that
/// never ran the rig is untouched. `CSR_NO_SYNTHETIC_FILTER=1` turns it off.
pub(crate) fn rig_sunk_ids(
    storage: &Storage,
    candidates: &[SinkRef<'_>],
    query: &str,
) -> HashSet<String> {
    rig_sunk_ids_if(filter_enabled(), storage, candidates, query)
}

fn rig_sunk_ids_if(
    enabled: bool,
    storage: &Storage,
    candidates: &[SinkRef<'_>],
    query: &str,
) -> HashSet<String> {
    let mut sunk = HashSet::new();
    if !enabled || candidates.is_empty() {
        return sunk;
    }
    let mut seen: HashSet<&str> = HashSet::new();
    let conversations: Vec<&str> = candidates
        .iter()
        .map(|chunk| chunk.conversation_id)
        .filter(|conversation| seen.insert(*conversation))
        .collect();
    let verdicts = session_verdicts(storage, &conversations);
    // Sub-agents whose own session is not an eval session: read their parents.
    // Ran-the-rig standing is weaker than a parent's eval verdict, so it does
    // not stand in for it.
    let mut parents: Vec<&str> = Vec::new();
    for verdict in verdicts.values() {
        if verdict.eval_session {
            continue;
        }
        if let Some(parent) = verdict.parent.as_deref() {
            if !verdicts.contains_key(parent) && !parents.contains(&parent) {
                parents.push(parent);
            }
        }
    }
    crate::mcp::stage_timing::count("synthetic_lineage_parents", parents.len() as u64);
    let parent_verdicts = session_verdicts(storage, &parents);
    for chunk in candidates {
        let Some(verdict) = verdicts.get(chunk.conversation_id) else {
            continue;
        };
        let parent = verdict
            .parent
            .as_deref()
            .and_then(|parent| parent_verdicts.get(parent).or_else(|| verdicts.get(parent)));
        let strong = verdict.eval_session || parent.is_some_and(|p| p.eval_session);
        let weak = strong || verdict.ran_rig || parent.is_some_and(|p| p.ran_rig);
        if (strong && verdict.rig_ids.contains(chunk.id))
            || (weak && crate::search::echo::quotes_query(chunk.content, query))
        {
            sunk.insert(chunk.id.to_string());
        }
    }
    sunk
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::ConversationChunk;
    use crate::provenance::ChunkProvenance;

    fn scan(seq: i64, marked: bool, bash: Option<&str>) -> EvalScanRow {
        EvalScanRow {
            id: format!("c{seq}"),
            seq,
            author: Some("assistant".into()),
            marked,
            output_marked: false,
            bash_text: bash.map(str::to_string),
            source_conv: None,
        }
    }

    fn put(storage: &Storage, conv: &str, seq: usize, author: Speaker, text: &str) -> String {
        let chunk = ConversationChunk {
            id: format!("{conv}-{seq}"),
            conversation_id: conv.into(),
            project_name: "p".into(),
            timestamp: "2026-10-01T00:00:00Z".into(),
            content: text.into(),
            message_count: 1,
            summary: None,
            author,
            seq,
            is_sidechain: false,
        };
        storage
            .insert_chunk_with_source(&chunk, &vec![0.1; 384], "conversation")
            .unwrap();
        storage
            .insert_chunk_provenance(
                &chunk.id,
                &ChunkProvenance {
                    author,
                    source_conv_id: conv.into(),
                    supersedes: None,
                },
            )
            .unwrap();
        chunk.id
    }

    fn refs(storage: &Storage, conv: &str, n: usize) -> Vec<ChunkRef> {
        (0..n)
            .map(|seq| {
                let id = format!("{conv}-{seq}");
                let author = storage.get_chunk_provenance(&id).unwrap().map(|p| p.author);
                ChunkRef {
                    id,
                    conversation_id: conv.into(),
                    author,
                }
            })
            .collect()
    }

    /// Eight rig sessions and two plain ones in a file store: enough
    /// conversations for the scan to split across lanes.
    fn lane_scan_fixture() -> (tempfile::TempDir, Storage, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("csr-engine.db")).unwrap();
        let bench = "[Bash: csr-engine bench --probe --db-path /x/.testbed/db]";
        let mut ids = Vec::new();
        for n in 0..8 {
            let id = format!("rig-{n}");
            for seq in 0..6 {
                put(&storage, &id, seq, Speaker::Assistant, bench);
            }
            ids.push(id);
        }
        for n in 0..2 {
            let id = format!("plain-{n}");
            put(&storage, &id, 0, Speaker::User, "how do I sort a vec");
            put(&storage, &id, 1, Speaker::Assistant, "use sort_by");
            ids.push(id);
        }
        (dir, storage, ids)
    }

    fn scan_shape(rows: &ScanRows) -> Vec<(String, usize)> {
        let mut shape: Vec<(String, usize)> =
            rows.iter().map(|(k, v)| (k.clone(), v.len())).collect();
        shape.sort();
        shape
    }

    #[test]
    fn a_failed_scan_lane_reads_the_whole_set_serially() {
        let (_dir, storage, ids) = lane_scan_fixture();
        let serial = storage
            .with_connection(|conn| {
                crate::storage::queries::conversation_eval_scan_many(
                    conn,
                    &ids,
                    MARKER_LIKE,
                    OUTPUT_LIKE,
                )
            })
            .unwrap();
        assert_eq!(serial.len(), 10);
        // The healthy lanes agree with the serial read.
        let healthy = scan_conversations(&storage, &ids).unwrap();
        assert_eq!(scan_shape(&healthy), scan_shape(&serial));
        // A lane that cannot open, one whose query fails, one that panics and
        // one with no connection each give the serial rows, none fewer.
        type LaneFailure = fn() -> anyhow::Result<Option<ScanRows>>;
        let failures: [(&str, LaneFailure); 4] = [
            ("open", || Err(anyhow::anyhow!("lane open failed"))),
            ("query", || Err(anyhow::anyhow!("lane query failed"))),
            ("none", || Ok(None)),
            ("panic", || panic!("lane panicked")),
        ];
        for (name, failure) in failures {
            let hit = std::sync::atomic::AtomicUsize::new(0);
            let scanned = scan_conversations_with(&storage, &ids, |_, _| {
                hit.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                failure()
            })
            .unwrap();
            assert!(hit.load(std::sync::atomic::Ordering::SeqCst) > 0, "{name}");
            assert_eq!(scan_shape(&scanned), scan_shape(&serial), "{name}");
        }
    }

    #[test]
    fn a_failed_scan_lane_still_drops_the_rig_sessions() {
        let (_dir, storage, ids) = lane_scan_fixture();
        // One lane cannot read `chunks` (a temp table shadows it there only).
        let scanned = scan_conversations_with(&storage, &ids, |lane_storage, group| {
            let lane = lane_storage.open_read_lane()?.expect("a file store");
            lane.with_connection(|conn| {
                conn.execute_batch("CREATE TEMP TABLE chunks (x TEXT);")?;
                Ok(())
            })?;
            lane.with_connection(|conn| {
                crate::storage::queries::conversation_eval_scan_many(
                    conn,
                    group,
                    MARKER_LIKE,
                    OUTPUT_LIKE,
                )
            })
            .map(Some)
        })
        .unwrap();
        let mut dropped = 0;
        for id in ids.iter().filter(|id| id.starts_with("rig-")) {
            let rows = scanned.get(id.as_str()).cloned().unwrap_or_default();
            assert!(judge_session(&rows).eval_session, "{id}");
            dropped += 1;
        }
        assert_eq!(dropped, 8);
    }

    #[test]
    fn a_recall_lane_is_read_only() {
        let (_dir, storage, _ids) = lane_scan_fixture();
        let lane = storage.open_read_lane().unwrap().expect("a file store");
        let write = lane.with_connection(|conn| {
            conn.execute("DELETE FROM chunks", [])?;
            Ok(())
        });
        assert!(write.is_err(), "a read lane accepted a write");
        let rows: i64 = lane
            .with_connection(|conn| {
                Ok(conn.query_row("SELECT count(*) FROM chunks", [], |r| r.get(0))?)
            })
            .unwrap();
        assert!(rows > 0);
    }

    #[test]
    fn a_failed_prefetch_scan_is_not_cached_as_a_clean_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("csr-engine.db")).unwrap();
        let bench = "[Bash: csr-engine bench --probe --db-path /x/.testbed/failscan]";
        for seq in 0..6 {
            put(&storage, "failscan-rig", seq, Speaker::Assistant, bench);
        }
        // The prefetch lane cannot read `chunks` (a temp table shadows it on
        // that connection alone), so its scan fails.
        let lane = storage.open_prompt_lane().unwrap().expect("a file store");
        lane.with_connection(|conn| {
            conn.execute_batch("CREATE TEMP TABLE chunks (x TEXT);")?;
            Ok(())
        })
        .unwrap();
        prefetch_verdicts(&lane, &["failscan-rig"]);
        // The consuming filter must scan again on its healthy connection and
        // drop the rig's rows, not trust a cached "clean".
        let candidates = refs(&storage, "failscan-rig", 6);
        let dropped = synthetic_ids_if(true, &storage, &candidates);
        assert_eq!(dropped.len(), 6, "{dropped:?}");
    }

    #[test]
    fn f2a_synthetic_batch_scan_matches_per_conversation() {
        let storage = Storage::open_memory().unwrap();
        // An eval session: bench calls in assistant chunks.
        let bench = "[Bash: csr-engine bench --probe --db-path /x/.testbed/db]";
        for seq in 0..6 {
            put(&storage, "rig", seq, Speaker::Assistant, bench);
        }
        // A normal session with a tool-result run printing CSR output.
        put(&storage, "plain", 0, Speaker::User, "how do I sort a vec");
        put(&storage, "plain", 1, Speaker::Assistant, "use sort_by");
        for seq in 2..6 {
            put(
                &storage,
                "plain",
                seq,
                Speaker::ToolResult,
                "mcp__claude-self-reflect__csr_reflect_on_past output",
            );
        }
        let ids: Vec<String> = ["rig", "plain", "absent"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let many = storage
            .with_connection(|conn| {
                crate::storage::queries::conversation_eval_scan_many(
                    conn,
                    &ids,
                    MARKER_LIKE,
                    OUTPUT_LIKE,
                )
            })
            .unwrap();
        assert!(!many.contains_key("absent"));
        for id in &ids {
            let single = storage
                .with_connection(|conn| {
                    crate::storage::queries::conversation_eval_scan(
                        conn,
                        id,
                        MARKER_LIKE,
                        OUTPUT_LIKE,
                    )
                })
                .unwrap();
            let batched = many.get(id).cloned().unwrap_or_default();
            assert_eq!(judge_session(&batched), judge_session(&single), "{id}");
            assert_eq!(batched.len(), single.len(), "{id}");
        }
        // Through the public entry: one call over three conversations.
        let mut candidates = refs(&storage, "rig", 6);
        candidates.extend(refs(&storage, "plain", 6));
        candidates.push(ChunkRef {
            id: "absent-0".into(),
            conversation_id: "absent".into(),
            author: Some(Speaker::User),
        });
        let dropped = synthetic_ids_if(true, &storage, &candidates);
        assert!(dropped.contains("rig-0") && dropped.contains("rig-5"));
        assert!(dropped.contains("plain-3"));
        assert!(!dropped.contains("plain-0") && !dropped.contains("absent-0"));
    }

    #[test]
    fn f1a_share_of_marked_chunks_makes_an_eval_session() {
        let rows: Vec<EvalScanRow> = (0..20).map(|i| scan(i, i < 6, None)).collect();
        assert!(judge_session(&rows).eval_session);
        // Four marked chunks in twenty: under the minimum.
        let rows: Vec<EvalScanRow> = (0..20).map(|i| scan(i, i < 4, None)).collect();
        assert!(!judge_session(&rows).eval_session);
        // Six marked chunks in 200: a long working session that quoted the rig.
        let rows: Vec<EvalScanRow> = (0..200).map(|i| scan(i, i < 6, None)).collect();
        assert!(!judge_session(&rows).eval_session);
    }

    #[test]
    fn f1a_bash_calls_that_run_the_rig_make_an_eval_session() {
        let bench = "[Bash: /x/bin/csr-engine-s9r bench --probe --db-path /x/db]";
        let probe = "[Bash: python3 -I s3/probe_ab.py compare --a a.json --b b.json]";
        let rows = vec![scan(3, false, Some(bench)), scan(9, false, Some(probe))];
        let verdict = judge_session(&rows);
        assert!(verdict.eval_session);
        // Two eval calls among forty Bash calls: a dev session that ran the rig.
        let filler = "[Bash: ls]".repeat(38);
        let mut rows = vec![scan(3, false, Some(bench)), scan(9, false, Some(probe))];
        rows.push(scan(12, false, Some(&filler)));
        let verdict = judge_session(&rows);
        assert!(!verdict.eval_session);
    }

    #[test]
    fn f1a_codex_bash_calls_count_too() {
        let call = "[external_agent_tool_call: Bash]\ncommand: cd /x/.testbed/core-rewrite && csr-engine bench --probe\n[/external_agent_tool_call]";
        let rows = vec![scan(1, false, Some(call)), scan(5, false, Some(call))];
        assert!(judge_session(&rows).eval_session);
    }

    #[test]
    fn f1a_tool_output_of_an_eval_call_drops_in_a_normal_session() {
        // One eval call among forty Bash calls: a normal session.
        let call = format!("[Bash: csr-engine eval --full]{}", "[Bash: ls]".repeat(39));
        let mut rows = vec![scan(10, false, Some(&call))];
        rows.extend((11..=14).map(|seq| result(seq, false)));
        rows.extend((15..30).map(|seq| scan(seq, false, None)));
        let verdict = judge_session(&rows);
        assert!(!verdict.eval_session);
        let chunk = |seq: i64, author| ChunkRef {
            id: format!("c{seq}"),
            conversation_id: "s".into(),
            author: Some(author),
        };
        assert!(dropped_by(&verdict, &chunk(11, Speaker::ToolResult)));
        assert!(dropped_by(&verdict, &chunk(12, Speaker::ToolResult)));
        assert!(!dropped_by(&verdict, &chunk(13, Speaker::ToolResult)));
        assert!(!dropped_by(&verdict, &chunk(11, Speaker::User)));
        assert!(!dropped_by(&verdict, &chunk(10, Speaker::Assistant)));
    }

    #[test]
    fn f1a_synthetic_ids_reads_the_session_not_the_row() {
        let storage = Storage::open_memory().unwrap();
        // A rig session: 12 chunks, 6 of them touch a .testbed path. The row
        // under test (seq 11) is plain prose with no marker of its own.
        for seq in 0..12 {
            let text = if seq < 6 {
                "ran it in /Users/x/.testbed/core-rewrite/wt and printed the scorecard"
            } else {
                "hooks use catch-all wrappers so a CSR bug never breaks the session"
            };
            put(&storage, "agent-rig", seq, Speaker::Assistant, text);
        }
        // A normal session with the same prose.
        for seq in 0..12 {
            put(
                &storage,
                "sess-normal",
                seq,
                Speaker::Assistant,
                "hooks use catch-all wrappers so a CSR bug never breaks the session",
            );
        }
        let mut candidates = refs(&storage, "agent-rig", 12);
        candidates.extend(refs(&storage, "sess-normal", 12));
        let dropped = synthetic_ids(&storage, &candidates);
        assert!(
            dropped.contains("agent-rig-11"),
            "prose row of a rig session"
        );
        assert_eq!(dropped.len(), 12, "{dropped:?}");
        assert!(!dropped.contains("sess-normal-3"));
    }

    #[test]
    fn f1a_synthetic_ids_drops_the_output_of_an_eval_call() {
        let storage = Storage::open_memory().unwrap();
        // A call at seq 4, its output at seq 5, an unrelated result at seq 9.
        put(
            &storage,
            "sess-dev2",
            4,
            Speaker::Assistant,
            "[Bash: csr-engine eval --full]",
        );
        put(
            &storage,
            "sess-dev2",
            5,
            Speaker::ToolResult,
            "all 20 tests passed",
        );
        put(
            &storage,
            "sess-dev2",
            9,
            Speaker::ToolResult,
            "unrelated listing",
        );
        for seq in [0usize, 1, 2, 3, 6, 7, 8] {
            put(&storage, "sess-dev2", seq, Speaker::User, "talk");
        }
        for seq in 10..40 {
            put(&storage, "sess-dev2", seq, Speaker::Assistant, "[Bash: ls]");
        }
        let candidates: Vec<ChunkRef> = [4usize, 5, 9]
            .iter()
            .map(|&seq| ChunkRef {
                id: format!("sess-dev2-{seq}"),
                conversation_id: "sess-dev2".into(),
                author: storage
                    .get_chunk_provenance(&format!("sess-dev2-{seq}"))
                    .unwrap()
                    .map(|p| p.author),
            })
            .collect();
        let dropped = synthetic_ids(&storage, &candidates);
        assert_eq!(dropped, HashSet::from(["sess-dev2-5".to_string()]));
    }

    #[test]
    fn f1a_kill_switch_keeps_everything() {
        let storage = Storage::open_memory().unwrap();
        for seq in 0..12 {
            put(
                &storage,
                "agent-rig2",
                seq,
                Speaker::Assistant,
                "in /Users/x/.testbed/core-rewrite",
            );
        }
        let candidates = refs(&storage, "agent-rig2", 12);
        assert_eq!(synthetic_ids_if(true, &storage, &candidates).len(), 12);
        let kept = synthetic_ids_if(false, &storage, &candidates);
        assert!(kept.is_empty());
    }

    fn result(seq: i64, printed: bool) -> EvalScanRow {
        EvalScanRow {
            id: format!("c{seq}"),
            seq,
            author: Some("tool_result".into()),
            marked: false,
            output_marked: printed,
            bash_text: None,
            source_conv: None,
        }
    }

    fn ids_of(rows: &[EvalScanRow]) -> HashSet<String> {
        judge_session(rows).output_ids
    }

    fn ids(range: std::ops::Range<i64>) -> HashSet<String> {
        range.map(|seq| format!("c{seq}")).collect()
    }

    #[test]
    fn f1a_a_run_that_prints_csr_output_is_dropped_whole() {
        // Six consecutive tool results, one of them prints a recall response.
        let rows: Vec<EvalScanRow> = (10..16).map(|i| result(i, i == 13)).collect();
        assert_eq!(ids_of(&rows), ids(10..16));
        // One in twenty is under the share.
        let rows: Vec<EvalScanRow> = (10..30).map(|i| result(i, i == 13)).collect();
        assert!(ids_of(&rows).is_empty());
        // Five printed chunks in a long run count outright; four do not.
        let rows: Vec<EvalScanRow> = (0..100)
            .map(|i| result(i, i % 20 == 0 && i < 100))
            .collect();
        assert_eq!(ids_of(&rows).len(), 100);
        let rows: Vec<EvalScanRow> = (0..100).map(|i| result(i, i % 25 == 0)).collect();
        assert!(ids_of(&rows).is_empty());
        // Two chunks are not a run.
        let rows = vec![result(4, true), result(5, true)];
        assert!(ids_of(&rows).is_empty());
        // A gap splits the run: the marked half drops, the other half stays.
        let mut rows: Vec<EvalScanRow> = (0..4).map(|i| result(i, i == 1)).collect();
        rows.extend((10..30).map(|i| result(i, false)));
        assert_eq!(ids_of(&rows), ids(0..4));
    }

    #[test]
    fn f1a_an_eval_session_keeps_its_tool_output() {
        let verdict = SessionVerdict {
            eval_session: true,
            output_ids: HashSet::from(["c45".to_string()]),
            ..SessionVerdict::default()
        };
        let chunk = |seq: i64, author| ChunkRef {
            id: format!("c{seq}"),
            conversation_id: "s".into(),
            author: Some(author),
        };
        // Prose goes, file reads and call output stay, a printed CSR run goes.
        assert!(dropped_by(&verdict, &chunk(3, Speaker::Assistant)));
        assert!(dropped_by(&verdict, &chunk(4, Speaker::User)));
        assert!(!dropped_by(&verdict, &chunk(11, Speaker::ToolResult)));
        assert!(!dropped_by(&verdict, &chunk(30, Speaker::ToolResult)));
        assert!(dropped_by(&verdict, &chunk(45, Speaker::ToolResult)));
    }

    #[test]
    fn f1a_synthetic_ids_drops_a_printed_recall_response() {
        let storage = Storage::open_memory().unwrap();
        for seq in 0..6 {
            let text = if seq == 2 {
                "mcp__claude-self-reflect__csr_reflect_on_past {\"query\": \"x\"}"
            } else {
                "rest of the printed response, plain words"
            };
            put(&storage, "sess-print", seq, Speaker::ToolResult, text);
        }
        for seq in 6..12 {
            put(&storage, "sess-print", seq, Speaker::Assistant, "talk");
        }
        let candidates = refs(&storage, "sess-print", 12);
        let dropped = synthetic_ids(&storage, &candidates);
        let expected: HashSet<String> = (0..6).map(|seq| format!("sess-print-{seq}")).collect();
        assert_eq!(dropped, expected);
    }
    // ---- rig_sunk_ids: demote by the session's provenance, never by prefix ----

    const QUERY: &str = "why do hooks use catch-all wrappers";
    const PLANTED: &str = "why do hooks use catch-all wrappers so they never block claude code? \
        [synthetic re-ask cycle 3] Because hooks are a side channel. Synthetic E3 test answer.";

    fn put_child(storage: &Storage, conv: &str, parent: &str, seq: usize, text: &str) -> String {
        let chunk = ConversationChunk {
            id: format!("{conv}-{seq}"),
            conversation_id: conv.into(),
            project_name: "p".into(),
            timestamp: "2026-10-01T00:00:00Z".into(),
            content: text.into(),
            message_count: 1,
            summary: None,
            author: Speaker::ToolResult,
            seq,
            is_sidechain: true,
        };
        storage
            .insert_chunk_with_source(&chunk, &vec![0.1; 384], "sidechain")
            .unwrap();
        storage
            .insert_chunk_provenance(
                &chunk.id,
                &ChunkProvenance {
                    author: Speaker::ToolResult,
                    source_conv_id: parent.into(),
                    supersedes: None,
                },
            )
            .unwrap();
        chunk.id
    }

    /// A working session that ran the rig twice among forty Bash calls: not an
    /// eval session by share, but it ran the rig.
    fn ran_rig_session(storage: &Storage, conv: &str) {
        put(
            storage,
            conv,
            0,
            Speaker::Assistant,
            "[Bash: csr-engine bench --probe --db-path /x/db]",
        );
        put(
            storage,
            conv,
            1,
            Speaker::Assistant,
            "[Bash: python3 s3/probe_ab.py compare a b]",
        );
        for seq in 2..42 {
            put(storage, conv, seq, Speaker::Assistant, "[Bash: ls]");
        }
    }

    fn sink_refs<'a>(rows: &'a [(String, String, String)]) -> Vec<SinkRef<'a>> {
        rows.iter()
            .map(|(id, conv, content)| SinkRef {
                id,
                conversation_id: conv,
                content,
            })
            .collect()
    }

    fn row(storage: &Storage, conv: &str, seq: usize, text: &str) -> (String, String, String) {
        let id = format!("{conv}-{seq}");
        let _ = storage;
        (id, conv.to_string(), text.to_string())
    }

    #[test]
    fn synth_a_planted_reask_row_of_a_session_that_ran_the_rig_sinks() {
        let storage = Storage::open_memory().unwrap();
        ran_rig_session(&storage, "sess-rig");
        let planted = put(&storage, "sess-rig", 50, Speaker::ToolResult, PLANTED);
        let rows = vec![row(&storage, "sess-rig", 50, PLANTED)];
        assert_eq!(rows[0].0, planted);
        let sunk = rig_sunk_ids_if(true, &storage, &sink_refs(&rows), QUERY);
        assert_eq!(sunk, HashSet::from([planted]));
    }

    #[test]
    fn synth_the_same_text_in_a_real_session_is_not_sunk() {
        let storage = Storage::open_memory().unwrap();
        // A real session: the user asked the question; no rig call anywhere.
        put(&storage, "sess-real", 0, Speaker::User, PLANTED);
        for seq in 1..20 {
            put(&storage, "sess-real", seq, Speaker::Assistant, "[Bash: ls]");
        }
        let rows = vec![row(&storage, "sess-real", 0, PLANTED)];
        let sunk = rig_sunk_ids_if(true, &storage, &sink_refs(&rows), QUERY);
        assert!(sunk.is_empty(), "{sunk:?}");
    }

    #[test]
    fn synth_a_bench_log_row_of_an_eval_session_sinks_and_its_file_reads_stay() {
        let storage = Storage::open_memory().unwrap();
        // 12 chunks, 6 of them rig material: an eval session by share.
        for seq in 0..6 {
            put(
                &storage,
                "agent-eval",
                seq,
                Speaker::ToolResult,
                "ran in /Users/x/.testbed/core-rewrite/wt",
            );
        }
        for seq in 6..12 {
            put(
                &storage,
                "agent-eval",
                seq,
                Speaker::ToolResult,
                "fn catch_all(hook: Hook) { /* source read */ }",
            );
        }
        let rows: Vec<_> = (0..12)
            .map(|seq| {
                let text = if seq < 6 {
                    "ran in /Users/x/.testbed/core-rewrite/wt"
                } else {
                    "fn catch_all(hook: Hook) { /* source read */ }"
                };
                row(&storage, "agent-eval", seq, text)
            })
            .collect();
        let sunk = rig_sunk_ids_if(
            true,
            &storage,
            &sink_refs(&rows),
            "unrelated question about sorting",
        );
        let expected: HashSet<String> = (0..6).map(|seq| format!("agent-eval-{seq}")).collect();
        assert_eq!(sunk, expected);
    }

    #[test]
    fn synth_a_sub_agent_takes_the_standing_of_the_session_that_spawned_it() {
        let storage = Storage::open_memory().unwrap();
        ran_rig_session(&storage, "sess-parent");
        // The child has no rig evidence of its own; its parent ran the rig.
        let quoted = put_child(&storage, "agent-child", "sess-parent", 0, PLANTED);
        let plain = put_child(
            &storage,
            "agent-child",
            "sess-parent",
            1,
            "a plain file read",
        );
        // Same text under a child of a session that never ran the rig.
        for seq in 0..10 {
            put(
                &storage,
                "sess-clean",
                seq,
                Speaker::Assistant,
                "[Bash: ls]",
            );
        }
        let other = put_child(&storage, "agent-other", "sess-clean", 0, PLANTED);
        let rows = vec![
            (
                quoted.clone(),
                "agent-child".to_string(),
                PLANTED.to_string(),
            ),
            (
                plain,
                "agent-child".to_string(),
                "a plain file read".to_string(),
            ),
            (other, "agent-other".to_string(), PLANTED.to_string()),
        ];
        let sunk = rig_sunk_ids_if(true, &storage, &sink_refs(&rows), QUERY);
        assert_eq!(sunk, HashSet::from([quoted]));
    }

    #[test]
    fn synth_a_sub_agent_that_ran_the_rig_still_takes_its_eval_parents_standing() {
        let storage = Storage::open_memory().unwrap();
        // The parent is an eval session by share and is not a candidate.
        for seq in 0..6 {
            put(
                &storage,
                "sess-evalp",
                seq,
                Speaker::ToolResult,
                "ran in /Users/x/.testbed/core-rewrite/wt",
            );
        }
        for seq in 6..12 {
            put(
                &storage,
                "sess-evalp",
                seq,
                Speaker::Assistant,
                "[Bash: ls]",
            );
        }
        // The child ran the rig twice among forty calls: weak standing only.
        ran_rig_session(&storage, "agent-weak");
        let marked = put_child(
            &storage,
            "agent-weak",
            "sess-evalp",
            50,
            "ran in /Users/x/.testbed/core-rewrite/wt",
        );
        let rows = vec![(
            marked.clone(),
            "agent-weak".to_string(),
            "ran in /Users/x/.testbed/core-rewrite/wt".to_string(),
        )];
        let sunk = rig_sunk_ids_if(
            true,
            &storage,
            &sink_refs(&rows),
            "unrelated question about sorting",
        );
        assert_eq!(sunk, HashSet::from([marked]));
    }

    #[test]
    fn synth_the_rig_tier_needs_the_query_and_the_kill_switch_keeps_all() {
        let storage = Storage::open_memory().unwrap();
        ran_rig_session(&storage, "sess-rig2");
        let unrelated = put(
            &storage,
            "sess-rig2",
            50,
            Speaker::ToolResult,
            "hooks wrap their bodies",
        );
        let planted = put(&storage, "sess-rig2", 51, Speaker::ToolResult, PLANTED);
        let rows = vec![
            (
                unrelated,
                "sess-rig2".to_string(),
                "hooks wrap their bodies".to_string(),
            ),
            (
                planted.clone(),
                "sess-rig2".to_string(),
                PLANTED.to_string(),
            ),
        ];
        let sunk = rig_sunk_ids_if(true, &storage, &sink_refs(&rows), QUERY);
        assert_eq!(sunk, HashSet::from([planted]));
        // A query that is not in the row: nothing sinks.
        let sunk = rig_sunk_ids_if(true, &storage, &sink_refs(&rows), "how do I sort a vec");
        assert!(sunk.is_empty());
        // Kill switch.
        assert!(rig_sunk_ids_if(false, &storage, &sink_refs(&rows), QUERY).is_empty());
    }
}
