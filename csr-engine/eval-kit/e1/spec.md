# E1 Ablation Binary Spec (five-part)

## 1. OBJECTIVE
New example binary `csr-engine/examples/saga_ablation.rs`: runs 20 provenance queries
through 7 retrieval arms against a frozen DB clone, one process = one index build,
emitting ranked conversation lists as JSONL for external scoring. Research harness like
`examples/saga_spike.rs` — NO production code changes, NO new deps.

## 2. FILES
- CREATE: `csr-engine/examples/saga_ablation.rs` (only file created/modified)
- REFERENCE (read, mirror logic exactly): `csr-engine/src/search/reinstatement.rs`
  (the production walk: seed selection lines 129-140, blend hop lines 402-428, graph
  spread 430-450, episode chain 452-465, fusion 468-497, rerank_pool 153-188, echo
  demotion 117-127), `csr-engine/examples/saga_spike.rs` (harness pattern: Engine::new,
  arm A construction, main loop).
- INPUT DB (runtime): env `CSR_ABLATION_DB` (no default — error if unset).
  Projects dir: env `CSR_ABLATION_PROJECTS` (empty dir; error if unset). Engine must
  never touch the live `~/.claude-self-reflect` DB.
- OUTPUT: env `CSR_ABLATION_OUT` path; JSONL, one line per (arm, query).

## 3. INTERFACES
Public crate API only (all verified pub): `csr_engine::engine::Engine::new(&db, &projects)`,
`engine.storage()`, `engine.embeddings().embed_single`, `engine.search().read().await`
with `.search_chunks / .search_chunks_filtered / .search_reflections`, storage methods
`get_chunks_by_ids, get_chunk_ids_for_conversation, get_chunk_vectors_by_ids,
files_for_session, sessions_for_file, get_reflections_by_two_tags, get_reflection_by_id,
get_chunk_provenance`, and `csr_engine::search::rerank::{rerank_with, RankCandidate,
RankPolicy}` with `RankPolicy::Provenance`.

Config constants (same as production defaults): K=10, SEEDS=3, BLEND_Q=0.65,
GRAPH_BOOST=1.10, GRAPH_CAP_PER_SEED=6, MIN_SCORE=0.20, W_QUERY_ECHO=0.35,
QUERY_ECHO_MIN_LEN=15, hop1 over-fetch = 2*K.

### Arms (channel flags over ONE shared walk implementation)
Implement walk once with flags {use_blend, use_graph, use_episode, use_rerank, use_echo}:
- `a_knn`: hop-1 only — chunks+reflections merged, score-sorted, truncate K. No hop-2,
  no rerank, no echo defense (mirrors saga_spike arm A exactly).
- `b_full`: all flags true — MUST mirror production `reinstate()` end-to-end: echo-aware
  seed selection, blend+graph+episode hop-2, max-score fusion, whole-pool detail fetch,
  W_QUERY_ECHO demotion inside rerank adapter, rerank_with(Provenance), truncate K.
- `c_blend_only`: use_blend only (graph/episode off), rerank+echo ON.
- `d_graph_only`: use_graph only, rerank+echo ON.
- `e_episode_only`: use_episode only, rerank+echo ON.
- `f_no_rerank`: blend+graph+episode ON, use_rerank=false (raw max-score fusion order),
  use_echo=false for seed selection too (raw top-N seeds).
- `g_no_echo`: blend+graph+episode+rerank ON, use_echo=false (plain top-N seeds, no
  W_QUERY_ECHO demotion in the rerank adapter).

Project scoping: none (project=None → search_chunks unfiltered) — matches E2 arm runs.

### Queries (exact texts + targets; qid order fixed)
Q1..Q12 = the 12 QUERIES in examples/saga_spike.rs lines 31-44 with same targets.
A1..A8 (targets used only in output metadata, not retrieval):
A1 "why did sign in switch from Clerk Core 3 finalize to legacy setActive in the expo app"
A2 "why does the expo app defer sign in with an auth intent service instead of prompting immediately"
A3 "why does the command center cache campaign data in a snapshot instead of calling the APIs live on page load"
A4 "why do returning user and anonymous user counts differ in the posthog numbers on the command center"
A5 "why was score save instrumented with observability across multiple app runtime versions"
A6 "why does the whats running section exist on the command center and what does it monitor"
A7 "why was the radio reel video built as a remotion composition with a root of multiple scenes"
A8 "why does the lessons page pull lesson analytics from posthog instead of supabase"

### Output format (JSONL, serde_json to_string per line)
{"arm":"b_full","qid":"Q1","convs":["<conv1>","<conv2>",...],
 "chunks":[{"id":"...","conv":"...","score":0.812,"via":"seed"},...]}
`convs` = distinct conversation_ids in final ranked order (first-occurrence dedupe of the
truncated top-K chunk list). `chunks` = the final K items. Also emit ONE header line first:
{"meta":{"db":"<path>","chunks_indexed":N,"built_at_unix":T}} for index-build provenance.

## 4. CONSTRAINTS
- Rust only, no new dependencies (serde_json, tokio, anyhow, dirs already available;
  check Cargo.toml [dev-dependencies] — examples use existing ones from saga_spike).
- NO type annotations violations — plain example binary, `#[tokio::main]` like spike.
- Do NOT modify src/**, Cargo.toml, or any existing file. Do NOT run cargo fmt on the
  whole tree (example file itself must be rustfmt-clean).
- Determinism: run all 7 arms per query inside one loop over queries, single Engine
  instance, single process. No timestamps in logic (output meta timestamp OK via
  std::time::SystemTime).
- Mirror-fidelity is the acceptance bar for b_full: same candidate flow as
  reinstatement.rs including reflection candidates in hop-1 pool, whole-pool detail
  fetch before rerank, retain-only-detailed, truncate AFTER rerank.

## 5. VERIFICATION (run it, include output in report)
export CSR_ABLATION_DB=$SCRATCH/e1/eval-clone/csr-engine.db
export CSR_ABLATION_PROJECTS=$SCRATCH/e1/eval-clone/empty-projects
export CSR_ABLATION_OUT=$SCRATCH/e1/ablation.jsonl
cd $HOME/projects/claude-self-reflect/csr-engine
source ~/.cargo/env
cargo build --release --example saga_ablation          # must compile clean
cargo run --release --example saga_ablation            # full run
wc -l $CSR_ABLATION_OUT                                 # expect 141 (1 meta + 7*20)
python3 -c "import json;[json.loads(l) for l in open('$CSR_ABLATION_OUT')]"  # all parse
Sanity: for >=15 of 20 queries, a_knn convs list != b_full convs list (channels do
something); every line has 1-10 convs; b_full for Q1 contains >1 distinct conv.
