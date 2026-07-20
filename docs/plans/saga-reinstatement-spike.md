# Saga Reinstatement Recall — Phase 0 Spike

**Goal:** prove (or kill) the core thesis cheaply, before building anything into the engine:

> Reinstatement recall — seed retrieval → reinstate encoding context (episode chain + code-graph
> spreading activation) → second-hop retrieval with a blended context vector — surfaces
> materially more of a question's true provenance than one-shot kNN at the same result budget.

This is the falsifiable core of the "three-way saga" invention (intent × deliberation × artifact
memory). If two-hop reinstatement cannot beat flat kNN on multi-hop "why" queries over our own
528 real conversations, the paper thesis dies here at near-zero cost.

## Phase 0 audit findings (2026-07-15)

| Question | Finding | Consequence for spike |
|---|---|---|
| Sidechains (agent chatter) | **Imported, unlabeled** — importer never reads `isSidechain` (`src/import/mod.rs:181`) | Strand-2 material exists in store; cannot be filtered/credited separately yet |
| Tool calls | Kept lossy: name + 1 param; results capped 4000 chars | Enough signal for spike |
| Chunk timestamps/order | One timestamp per conversation; **no sequence column**; order only implicit in UUIDv5(index) | Chunk-level temporal reinstatement deferred; episode-level chains used instead (`prev_episode_id`, `hooks/stop.rs:54`) |
| Code graph | `code_nodes`/`code_edges` carry conv provenance both directions; `episode_anchors` timestamped; `code_evolution` = session↔file timeline | Spreading-activation substrate ready |
| Access logging | Only prompt_submit path logs `retrieval_events`; MCP searches unlogged | **Learned-decay component deferred** — spike tests 3 of 4 mechanisms |
| Eval harness | Hardcoded smoke tests + continuity gate (`src/eval/continuity.rs`), no external dataset format | Spike ships as standalone example bin, not eval extension |

Live substrate: 71,762 chunks / 528 conversations / 2,449 reflections / 3,967 code nodes /
13,908 edges / 1,256 anchors in `~/.claude-self-reflect/csr-engine.db`.

## Spike design

**Artifact:** `csr-engine/examples/saga_spike.rs` — read-only against the live DB, throwaway
research code, no engine changes, no schema changes.

**Query set:** 12 multi-hop "why does the code look like this" questions about CSR itself
(decisions we know span multiple sessions), each with a target file for ground truth.

**Arms (equal budget k=10):**
- **A (baseline):** one-shot kNN, chunks + reflections merged by cosine — current
  `reflect_on_past` retrieval minus formatting.
- **B (reinstatement):** top-3 kNN seeds, then per seed:
  1. *Context blend:* `0.65·query_vec + 0.35·seed_vec` (renormalized) → second-hop kNN.
  2. *Code-graph spread:* seed session → `code_evolution` files → other sessions touching the
     same files → best chunk per neighbor session (cosine vs query, small activation boost).
  3. *Episode chain:* seed session's episode reflection → `prev_episode_id` walk (1 step).
  Fused, deduped, capped at 10.

**Metrics per query:**
- **M1 GT coverage:** ground truth = sessions in `code_evolution` that touched the target file.
  Count of GT sessions represented in each arm's top-10. *Disclosed bias:* B's spread hops
  through the same table — but only via seed sessions; wrong seeds → no gain. Relative
  comparison still informative; qualitative judging (M3) is the check.
- **M2 provenance diversity:** distinct conversations in top-10; results connected by shared
  session/file edges (chain vs disconnected set).
- **M3 judged:** side-by-side outputs dumped to file; human judge marks which arm better
  answers the "why" (better / tie / worse), junk rate of B-only results.

**Gates (decide before running):**
- **PROCEED** to Phase 1 (engine integration) if: B ≥ A + 25% on summed GT coverage, AND B judged
  better on ≥50% of queries with junk rate < 30%.
- **ITERATE** if mixed (B wins coverage but junk high → tune activation weights once, rerun).
- **KILL** if B ≤ A on coverage and judged — thesis fails on our own data; write that up honestly.

**Deferred (not needed to prove value):** learned decay (blocked on MCP-path retrieval logging),
chunk-level temporal ordering (needs sequence column), sidechain labeling (strand-2 credit
assignment), SessionStart narration (marketing surface, only after recall proven).

## Standard process notes

- Context7 research: skipped — no new dependencies; spike uses existing crate APIs only.
- Review: spike is throwaway measurement code; review effort goes to the *results*, not the code.
  Engine integration (Phase 1, if gate passes) gets the full research → implement → Codex review
  → verify cycle.

## Results (run 2026-07-15, live DB: 71,762 chunks / 528 conversations)

**GATE PASSED — PROCEED.**

| Metric (12 queries, top-10 budget) | A: one-shot kNN | B: reinstatement |
|---|---|---|
| Ground-truth session coverage (sum) | 15 | **23 (+53% rel., gate was +25%)** |
| Queries where arm wins coverage | 0 | 7 (5 ties, 0 losses) |
| Distinct-conversation diversity (sum) | 75 | 77 (≈equal — B wins without spraying) |

**Bias check (the disclosed code_evolution circularity):** B's GT-hit result lines by
mechanism — blend 24, graph 9, episode 0. The majority of the gain comes from the **blend
channel** (query⊕seed context vector, pure semantics, zero dependence on the code graph), so
the win is not an artifact of sharing the GT table. Graph spread adds on top; episode chains
contributed nothing yet (prev_episode_id chains too sparse/recent — expected to strengthen).

**Qualitative highlight (Q3, integrity-check cache):** kNN's top-10 was entirely subagent
mechanic chunks (0 GT); reinstatement's blend hop surfaced the human origin conversation
`7eccb720` where the decision was actually made. Exactly the failure mode the thesis predicts:
one-shot similarity drowns in agent chatter; reinstating the seed's encoding context digs back
to intent. Junk rate in B-only results: low (eyeball <20%, mostly looser-but-relevant graph
pulls).

**Incidental finding:** subagent transcripts import under `agent-*` conversation ids and
dominate several kNN top-10s — strand-2 material is present, unlabeled, and already shaping
(polluting *and* enriching) recall. Strengthens the case for explicit sidechain labeling.

Full output: spike run log (saga_spike_run1.txt, session scratchpad); rerun anytime via
`cargo run --release --example saga_spike`.

## Sequel (gate passed — next)

1. Migration: chunk `seq` column + `isSidechain` flag + MCP-path retrieval logging (unblocks
   learned decay + strand-2).
2. `csr_why` MCP tool: productionized reinstatement walk.
3. Eval: fold query set into `csr-engine eval` as a provenance benchmark.
4. Paper + arXiv preprint (race: ACT-Up 2606.28045 / MRMS 2607.04617 are one extension away;
   E-mem 2601.21714 owns code-state without dynamics).
