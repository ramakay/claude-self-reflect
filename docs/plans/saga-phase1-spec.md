# Saga Phase 1 Spec — Capture Migration + `csr_why` Reinstatement Tool + Provenance Eval

Status: ADVISOR-REVIEWED (grok, 2026-07-15: "patch 3 gaps, then WS1→WS2 sequential" — all
pins folded in below) → implement (grok lane, WS1 then WS2) → DoD verification on this machine.
Prereq: Phase 0 spike PASSED (docs/plans/saga-reinstatement-spike.md, +53% provenance coverage).

Advisor pins incorporated (deciding risk verified in code: `search_chunks_filtered` with a tiny
per-conversation id set escalates to near-full-index HNSW search, `src/search/mod.rs:316-345`):
exact per-conversation scoring helper replaces filtered search; MCP logging uses session_id
sentinel `"mcp"`; eval gate is opt-in local, graceful on missing DB; sidechain rule = ANY
message OR `agent-*`; limit semantics pinned; WS1 must ship all storage helpers WS2 needs.

## 1. Objective

Productionize reinstatement recall inside csr-engine:

- **WS1 — capture migration:** chunk `seq` + `is_sidechain` columns, populated on import and
  backfillable for the existing DB; retrieval-event logging on the MCP search path.
- **WS2 — recall:** `src/search/reinstatement.rs` module implementing the proven walk
  (seed → blend → code-graph spread → episode chain), exposed as new MCP tool `csr_why`
  (14th tool), plus `csr-engine eval --provenance` benchmark folding in the 12 spike queries.

Non-goals (Phase 2+): learned decay (needs accumulated MCP retrieval logs — this phase only
starts collecting), chunk-level temporal reinstatement (needs seq history to accrue),
SessionStart saga narration, strand-2 rerank weighting (this phase only labels sidechains).

## 2. Files

| File | Change |
|---|---|
| `csr-engine/src/storage/migrations.rs` | Idempotent `ALTER TABLE chunks ADD COLUMN seq INTEGER` + `ADD COLUMN is_sidechain INTEGER NOT NULL DEFAULT 0` (follow the existing `summary`-column ALTER pattern) |
| `csr-engine/src/import/mod.rs` | `ConversationChunk` gains `seq: usize`, `is_sidechain: bool`. Populate `seq` from the existing chunk index `i` (already feeds UUIDv5). Parse `isSidechain` per JSONL line; a chunk is sidechain if **any** of its messages is sidechain OR the conversation id starts with `agent-` (matches the agent-pollution finding: over-label beats under-label for later credit assignment). |
| `csr-engine/src/storage/queries.rs` | `insert_chunk` writes both columns (cast usize→i64). New queries WS2 depends on — ALL ship in WS1: `get_chunk_ids_for_conversation(conv_id)`, `get_chunk_vectors_by_ids(&[String]) -> Vec<(String, Vec<f32>)>` (SELECT from `chunk_embeddings` by id list, chunked IN-clauses), `sessions_for_file(file_suffix, exclude_session, limit)` and `files_for_session(session_id, limit)` over `code_evolution` (lift from spike). Backfill query: `UPDATE chunks SET seq=?, is_sidechain=? WHERE id=?`. |
| `csr-engine/src/import/backfill.rs` (or sibling) | `backfill_saga_columns(engine)` — for each imported file still on disk: re-parse, recompute deterministic UUIDv5 chunk ids, UPDATE seq/is_sidechain. Wire into existing backfill/daemon entry point + a CLI path (`csr-engine import --backfill-saga` or equivalent existing subcommand pattern). Missing JSONL: skip, leave `seq=NULL`/`is_sidechain` as-is, report skipped count (never silent). |
| `csr-engine/src/mcp/tools.rs` | (a) `reflect_on_past` logs returned memory ids via `log_retrieval_event` with `hook_phase="mcp_search"`, `session_id="mcp"` (sentinel — MCP has no session id; distinguishable for Phase-2 decay work; NOT empty string). Batch, non-fatal on error. (b) New tool fn `why` calling WS2 module; same logging. |
| `csr-engine/src/mcp/` server registration | Register `csr_why` with `Parameters<WhyParams>` pattern, annotations `read_only_hint=true, destructive_hint=false, idempotent_hint=true`. Tool count 13→14. |
| `csr-engine/src/search/reinstatement.rs` (new) | Core walk, pure library code, unit-testable. |
| `csr-engine/src/eval/provenance.rs` (new) + `src/eval/mod.rs` + `src/main.rs` | `eval --provenance`: opt-in LOCAL gate (never part of default `eval`/`eval --full`, never CI). Runs 12 spike queries, arm A (one-shot kNN) vs arm B (reinstatement), prints per-query + summary coverage. Missing/empty live DB or zero GT sessions → print "provenance eval skipped: <reason>", exit 0. Exit nonzero ONLY when both arms ran and summed B coverage < A. |
| `CLAUDE.md` (repo root) | Tool list 13→14 (`csr_why` line), hooks/commands unchanged. |

## 3. Interfaces

```rust
// src/search/reinstatement.rs
pub struct ReinstateConfig {
    pub k: usize,            // result budget, default 10
    pub seeds: usize,        // default 3
    pub blend_query_weight: f32, // default 0.65
    pub graph_boost: f32,    // default 1.10
    pub graph_cap_per_seed: usize, // default 6
    pub min_score: f32,      // default 0.20
}

pub struct EvidenceItem {
    pub chunk_id: String,
    pub conversation_id: String,
    pub score: f32,
    pub via: Via,            // Seed | Blend | Graph | Episode | Reflection
    pub timestamp: String,
    pub excerpt: String,     // ~200 chars, cleaned
}

/// The proven walk. Async only because SearchEngine sits behind tokio RwLock.
pub async fn reinstate(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    project: Option<&str>,
    cfg: &ReinstateConfig,
) -> Result<Vec<EvidenceItem>>;
```

```rust
// MCP tool params (schemars v1 derive, rmcp Parameters<T> pattern)
pub struct WhyParams {
    /// The "why/how did this come to be" question.
    pub query: String,
    /// Max evidence items (default 10).
    pub limit: Option<usize>,
    /// Project scope (same semantics as csr_reflect_on_past).
    pub project: Option<String>,
}
```

`csr_why` output format (text, mirrors existing tools): header line with query + timings, then
evidence items grouped by conversation, chronological within group, each line carrying
`via`, score, timestamp, `conv_<id>` retrieval handle, excerpt. Footer: distinct conversation
count + "chain" summary (which conversations were reached by graph/episode hops from which
seeds).

Per-conversation best-chunk selection inside the walk MUST use exact scoring over that
conversation's stored embeddings: `get_chunk_ids_for_conversation` →
`get_chunk_vectors_by_ids` → cosine vs query in plain Rust, take best. Do NOT use
`SearchEngine::search_chunks_filtered` here (tiny allowed-id sets escalate it to a near
full-index HNSW search, `src/search/mod.rs:316-345` — blows the latency budget), and do NOT
load all 71k vectors like the spike did. Conversations average ~136 chunks; exact cosine over
that is microseconds.

`limit` semantics: `None` → 10; `Some(0)` → empty result (valid, not an error); values are
capped at 50.

## 4. Constraints

- rmcp ~1.6: `Parameters<MyStruct>`, schemars v1, annotations in macro — copy an existing tool.
- rusqlite 0.40: no `ToSql` for `usize` — cast to `i64`.
- Storage: `Connection` behind `std::sync::Mutex` — all new queries go through
  `storage/queries.rs` + a `Storage` wrapper method, matching existing style.
- Migrations must be idempotent (re-run safe) and additive only — no table rebuilds; existing
  rows get `seq=NULL`, `is_sidechain=0` until backfill.
- Retrieval logging must be **non-fatal**: a logging error never fails the search. Never block.
- `csr_why` latency target: <100ms warm (no LLM anywhere in the path).
- Old DBs without backfill must still work: `csr_why` and eval treat `seq=NULL` as unknown,
  never panic, never filter on `is_sidechain` (labeling only this phase).
- Keep `examples/saga_spike.rs` compiling (it's the historical record; do not refactor it).
- Style: match surrounding code; comments only for constraints code can't show.
- No new dependencies.

## 5. Verification (DoD — every box checked on this machine before merge)

```bash
cd /Users/ramakrishnanannaswamy/projects/claude-self-reflect/csr-engine
cargo fmt --check && cargo clippy --all-targets -- -D warnings
cargo test                          # 504 unit + integration suites, all green
cargo build --release
```

1. **Migration:** fresh in-memory DB has both columns; re-running migrations is a no-op
   (unit test). Live DB after `--backfill-saga`: `seq` non-null for conversations whose JSONL
   still exists; `agent-*` conversations have `is_sidechain=1` (verify with sqlite3 query).
2. **Logging:** calling `csr_reflect_on_past` inserts `retrieval_events` rows with
   `hook_phase='mcp_search'` (integration test on temp DB + manual check on live DB).
3. **`csr_why`:** registered (tool count 14 in status/eval), returns cited evidence chain for
   "why is integrity check cached" including conversation `7eccb720` on this machine's DB,
   <100ms warm.
4. **Eval:** `csr-engine eval --provenance` prints A vs B per-query coverage and summary;
   B ≥ A on this machine (spike replication through production code path); nonzero exit on
   regression.
5. **Docs:** CLAUDE.md tool count + tool list updated; this spec updated with results.
6. No `--no-verify`, no test skips, no "however".

## 6. Findings this spec must not lose (Phase 0, 2026-07-15)

- Reinstatement beat kNN 23 vs 15 GT-session coverage (+53%, gate +25%), 7W/5T/0L, equal
  diversity, on 12 multi-hop "why" queries over the live DB (71,762 chunks / 528 convs).
- Unbiased blend channel drove the win (24 GT-hit lines vs graph 9, episode 0).
- Poster case Q3: kNN top-10 = all subagent mechanic chunks, 0 GT; reinstatement rescued the
  human origin conversation `7eccb720`.
- Sidechains import unlabeled as `agent-*` conversations and dominate several kNN top-10s.
- Episode chains contributed 0 — too sparse yet; expected to strengthen as sessions accrue.
- Artifacts: `docs/plans/saga-reinstatement-spike.md`, `csr-engine/examples/saga_spike.rs`,
  commit `800636f` on `saga-phase1`; full run log in session scratchpad (saga_spike_run1.txt);
  CSR reflection stored (id 9810c8b8).

## 7. Pipeline after this phase

DoD pass → findings section appended here → paper draft ("Three-Trace Sagas: joint episodic
memory over intent, deliberation, and artifact in agentic software construction"; grounding:
CMR/TCM Howard & Kahana 2002, Polyn 2009; ACT-R Anderson & Schooler 1991; Naur 1985; race:
ACT-Up 2606.28045, MRMS 2607.04617, E-mem 2601.21714) → release (minor version, npm + docs).
