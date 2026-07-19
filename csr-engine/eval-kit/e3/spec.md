# E3 Controlled Contamination — Five-Part Spec (grok-implementer executes end-to-end)

## 1. OBJECTIVE
Measure how corpus contamination (eval-design transcript, sham transcript, scripted re-ask
cycles) displaces origin conversations in retrieval. Build 4 condition DBs from the C0
snapshot, write + run an exact-brute-force retrieval binary over 8 eligible queries × 3
arms × 4 conditions, emit raw JSONL for external scoring. Exact scan kills ANN variance —
the independent variable is corpus CONTENT only.

## 2. FILES
- CREATE: `csr-engine/examples/saga_contamination.rs` (ONLY repo file created/modified).
- CREATE (scratchpad, not repo):
  `SCRATCH/e3/conditions/{c0,c1,csham,c5}/csr-engine.db` — condition DBs
  `SCRATCH/e3/conditions/{c1,csham,c5}/projects/<dir>/...jsonl` — payload transcripts
  `SCRATCH/e3/c5_transcripts/` — 5 generated synthetic JSONLs
  `SCRATCH/e3/out/{c0,c1,csham,c5}.jsonl` — binary outputs
  where SCRATCH = $SCRATCH
- READ-ONLY inputs:
  C0 source: $HOME/.claude-self-reflect/backups/pre-test-episode-sweep-20260611-123810/csr-engine.db (NEVER modify the backup; cp per condition)
  C1 payload: $HOME/.claude/projects/-Users-USER-projects-claude-self-reflect-csr-engine/a4635d59-b028-4d45-a68f-1ccc5fb712b0.jsonl (the eval-design/spike session — verify it exists; if missing, STOP and report)
  Reference code: csr-engine/src/search/reinstatement.rs (walk to mirror), csr-engine/examples/saga_ablation.rs (harness pattern, ArmFlags idea)

## 3. INTERFACES

### Conditions
- c0: pristine copy of the backup DB. Empty projects dir (no import).
- c1: copy + import of ONLY the a4635d59 transcript (place it under projects/<same dir name as its real parent dir>/).
- csham: copy + import of ONE unrelated transcript, size-matched: pick a .jsonl from
  $HOME/.claude/projects/-Users-USER-projects-cc-enhance/ (or anukriti-website dir if cc-enhance has no fit) whose byte size is 70-130% of the c1 payload AND which contains ZERO occurrences of any of the 8 query texts below (grep -c each, all must be 0). Report the chosen file + size ratio.
- c5: copy + import of 5 synthetic re-ask transcripts you generate: files
  SCRATCH/e3/c5_transcripts/c5cycle{1..5}-e3000000-0000-4000-8000-00000000000{1..5}.jsonl.
  Each mimics real JSONL structure (copy the line-shape of a real transcript: lines with
  {"type":"user","message":{"role":"user","content":[{"type":"text","text":...}]},"timestamp":...}
  and assistant counterparts — inspect the c1 payload's first lines and mirror the schema
  exactly, minus tool calls). Each cycle file: for EACH of the 8 queries, one user turn
  asking the query text VERBATIM + one assistant turn of 2-3 sentences paraphrasing an
  answer (vary wording per cycle). Timestamps: cycle N uses 2026-06-1{1+N}T10:00:00Z
  onward, incrementing seconds per turn.

### Import mechanism
In the binary (or a --import-only mode): `Engine::new(&db, &projects_dir)` then
`engine.import_conversations(None).await` — run once per condition BEFORE scanning
(c0's projects dir is empty so import is a no-op there). Log imported-count per condition.

### Binary: examples/saga_contamination.rs
Env: CSR_E3_DB, CSR_E3_PROJECTS, CSR_E3_OUT (all required, error if unset).
Flow: Engine::new → import_conversations(None) → load_all_chunk_vectors() once into
memory → for each of 8 queries, run 3 arms, write JSONL.

Arms (exact scan only — NEVER call engine.search()'s HNSW methods):
- knn_exact: cosine(query_vec, every chunk vec), top-10 by score (min_score 0.20).
- full_exact: the reinstatement walk mirrored from reinstatement.rs BUT every
  chunk search replaced by exact scan (hop-1 seed search = exact top-20; per-seed blend
  hop = exact top-5 of the blended vector); graph spread + episode chain unchanged (they
  already use exact per-conv cosine); echo-aware seed selection ON, W_QUERY_ECHO demotion
  + rerank_with(RankPolicy::Provenance) ON. Constants same as ablation: K=10, SEEDS=3,
  BLEND_Q=0.65, GRAPH_BOOST=1.10, GRAPH_CAP_PER_SEED=6, MIN_SCORE=0.20, W_QUERY_ECHO=0.35.
  OMIT the reflection channel entirely (C0-era reflections sparse; disclosed upstream).
- full_no_echo_exact: same as full_exact with echo defense fully OFF (plain top-N seeds,
  no W_QUERY_ECHO demotion) — the repair-delta arm.

Output JSONL per (arm, query):
{"arm":"full_exact","qid":"Q5","convs":["..."],"chunks":[{"id":"..","conv":"..","score":0.71,"via":"seed","echo":true}]}
`echo` = chunk content (lowercased) contains the full query text (lowercased).
Header line: {"meta":{"db":"...","chunks":N,"imported":M}}.

### Queries (8 eligible — origins pre-date C0; qid keys fixed)
Q5 "why does import skip conversations that start with CSR agent prompts"
Q9 "why do hooks use catch-all wrappers so they never block claude code"
Q12 "why was fts5 keyword fallback added when semantic scores are low"
A3 "why does the command center cache campaign data in a snapshot instead of calling the APIs live on page load"
A5 "why was score save instrumented with observability across multiple app runtime versions"
A6 "why does the whats running section exist on the command center and what does it monitor"
A7 "why was the radio reel video built as a remotion composition with a root of multiple scenes"
A8 "why does the lessons page pull lesson analytics from posthog instead of supabase"

## 4. CONSTRAINTS
- Only repo file touched: examples/saga_contamination.rs. No src/**, no Cargo.toml edits,
  no new deps. rustfmt-clean, clippy-clean for the example.
- NEVER write to the backup or to ~/.claude-self-reflect/csr-engine.db (live DB).
- Condition DBs live only under SCRATCH/e3/conditions/.
- Synthetic transcripts must be clearly synthetic in content (assistant text may say so)
  but structurally valid for import.
- Run conditions SEQUENTIALLY (each Engine::new loads the embedding model; avoid RAM spikes).

## 5. VERIFICATION (run everything, include outputs in report)
cd $HOME/projects/claude-self-reflect/csr-engine && source ~/.cargo/env
cargo build --release --example saga_contamination   # clean
For each condition c0,c1,csham,c5 (exact env values):
  CSR_E3_DB=SCRATCH/e3/conditions/<c>/csr-engine.db \
  CSR_E3_PROJECTS=SCRATCH/e3/conditions/<c>/projects \
  CSR_E3_OUT=SCRATCH/e3/out/<c>.jsonl cargo run --release --example saga_contamination
Checks (script them):
- 4 output files, each 25 lines (1 meta + 3 arms × 8 queries), all JSON-parse.
- meta.imported: c0=0, c1=1, csham=1, c5=5.
- c1/c5 meta.chunks > c0 meta.chunks (payload actually imported and embedded).
- In c5 output, at least one arm/query has an echo=true chunk in top-10 (the injected
  re-asks are retrievable) — if zero echoes anywhere in c5, the injection failed: report.
- Report per-condition: chunks total, imported count, and for Q5 the knn_exact top-3 convs.
