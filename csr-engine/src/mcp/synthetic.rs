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
    }
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

fn session_verdict(storage: &Storage, conversation_id: &str) -> Arc<SessionVerdict> {
    {
        let held = cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, verdict)) = held.get(conversation_id) {
            if at.elapsed() < VERDICT_TTL {
                return Arc::clone(verdict);
            }
        }
    }
    // A failed read is a pass: the filter never removes what it cannot judge.
    let verdict = Arc::new(
        storage
            .with_connection(|conn| {
                crate::storage::queries::conversation_eval_scan(
                    conn,
                    conversation_id,
                    MARKER_LIKE,
                    OUTPUT_LIKE,
                )
            })
            .map(|rows| judge_session(&rows))
            .unwrap_or_default(),
    );
    let mut held = cache().lock().unwrap_or_else(|e| e.into_inner());
    if held.len() >= CACHE_CAP {
        held.clear();
    }
    held.insert(
        conversation_id.to_string(),
        (Instant::now(), Arc::clone(&verdict)),
    );
    verdict
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
    let mut verdicts: HashMap<&str, Arc<SessionVerdict>> = HashMap::new();
    for chunk in candidates {
        let verdict = verdicts
            .entry(chunk.conversation_id.as_str())
            .or_insert_with(|| session_verdict(storage, &chunk.conversation_id));
        if dropped_by(verdict, chunk) {
            dropped.insert(chunk.id.clone());
        }
    }
    dropped
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
}
