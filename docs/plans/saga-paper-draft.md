# Three-Trace Sagas: Joint Episodic Memory over Intent, Deliberation, and Artifact in Agentic Software Construction

## Abstract

Agentic coding sessions leave three correlated traces that most memory systems index separately or not at all: *intent* (human prompts), *deliberation* (agent reasoning and tool calls), and *artifact* (code changes). We call the joint structure spanning these traces for one unit of work a *saga*. This paper presents claude-self-reflect (CSR), a single Rust binary that jointly indexes Claude Code session transcripts, local embeddings, and an AST-derived code graph, and introduces *reinstatement recall*—a multi-hop retrieval algorithm, exposed as the MCP tool `csr_why`, that reinstates encoding context across a saga rather than matching a query with one-shot nearest neighbors.

On a live corpus of 71,762 chunks, 528 conversations, and a code graph of 3,967 nodes / 13,908 edges, a pre-registered spike evaluation of 12 multi-hop provenance questions (“why does this code look like this?”) found that reinstatement raised summed ground-truth session coverage from 15 to 23 at k=10 (+53% relative; gate threshold +25%), winning 7/12 queries and tying 5/12 with zero losses. A same-day production reimplementation on a 20-query suite scored 15 → 18/19 under partial self-contamination. Mechanism attribution shows most of the gain comes from a pure-semantic context-blend step (a computational analogue of the Temporal Context Model), not graph walking. We also report a Heisenberg-like *observer effect*: evaluating a self-indexing memory system against a live corpus lets the evaluation session’s own transcripts displace origin conversations. A follow-up provenance-aware rerank of the reinstatement pool—centrally a *query-echo* defense that demotes chunks quoting the query verbatim and prefers non-echo seeds—held aggregate coverage while repairing the canonical acceptance-failure query (0 → 1 ground-truth sessions) on a frozen corpus. We argue that frozen pre-evaluation snapshots and provenance-aware demotion of self-echo are necessary methodological controls for any self-recording agent memory.

## Introduction

In *Programming as Theory Building*, Naur argued that source code is not the primary artifact of software work. The real artifact is the *theory* in programmers’ heads—the web of intentions, constraints, and rejected alternatives that explains why the code has the shape it does. When those people leave or forget, the theory is lost, and later maintainers inherit a brittle surface that no longer answers “why” (Naur, 1985).

Agentic software construction sharpens Naur’s problem. The “programmer” is no longer a stable human team but a transient human–agent collaboration stretched across many sessions, subagents, and tool traces. Intent is expressed in prompts; deliberation unfolds as reasoning and tool calls (including dead ends and sidechain chatter); artifact materializes as file-level edits. The theory that would justify a later design decision is distributed across those three traces and is *more* perishable than classical human theory-building: sessions end, context windows compact, subagent transcripts fragment into separate files, and the human who held the intent may never re-open the originating conversation.

Most retrieval systems for coding agents treat memory as a flat bag of text: embed utterances, retrieve top-*k* by cosine similarity, and hope the right chunk appears. For *lexical* or *local* questions (“where is `integrity_check` defined?”), this often works. For *provenance* questions—“why is the integrity check cached?”, “why was this API shape chosen?”—flat nearest-neighbor search systematically fails in a predictable way. The query is textually closest to agent restatements, tool-mechanic chatter, or later sessions that *discuss* the decision, not to the human-origin conversation where the decision was made. Similarity drowns intent.

This paper’s contribution is a joint *three-trace* memory design and a retrieval algorithm that walks a *saga*—the correlated intent–deliberation–artifact structure for one unit of work. We implement the system as claude-self-reflect (CSR): a single Rust binary with local MiniLM embeddings (384-dim, FastEmbed), HNSW vector search, SQLite storage, ast-grep code graph construction, fourteen MCP tools, and six Claude Code lifecycle hooks that capture sessions as they happen. The provenance path is reinstatement recall (`csr_why`): seed with one-shot kNN, blend query and seed context for a second hop, spread activation over a session–file code graph, hop one step along episode chains, then fuse and cap.

We evaluate on dogfooded multi-hop questions about CSR’s own development history, with pre-registered proceed/iterate/kill gates. The spike passes; production reimplementation is faithful but lower under same-day corpus contamination. That discrepancy is itself a finding: self-indexing memory systems exhibit an observer effect that evaluation design must confront.

## Related Work

**Temporal context and reinstatement.** Howard and Kahana’s Temporal Context Model (TCM) proposes that episodic recall is driven not only by item–item similarity but by reinstatement of a drifting context representation that bound items together at encoding (Howard & Kahana, 2002). Our context-blend step—forming \(0.65\cdot q + 0.35\cdot s\) from query vector \(q\) and seed vector \(s\), renormalizing, and re-searching—is a deliberate computational analogue: retrieval reinhabits the contextual neighborhood of a retrieved memory rather than matching the literal cue alone. The Context Maintenance and Retrieval (CMR) model extends TCM with source and organizational structure (Polyn, Norman, & Kahana, 2009). CMR motivates treating intent, deliberation, and artifact as distinct but contextually bound sources; our Phase-1 system still fuses them into a single blend rather than source-factored reinstatement, which we flag as future work.

**Rational analysis of memory access.** Anderson and Schooler’s rational analysis argues that human memory’s sensitivity to recency and frequency mirrors a Bayesian estimate of future need given environmental statistics (Anderson & Schooler, 1991). This supplies a theoretical warrant for *learned decay*: weighting retrieval by logged access patterns rather than fixed similarity alone. CSR previously logged retrieval only on the prompt-submit hook path; Phase 1 adds MCP-path logging so usage-weighted reinstatement becomes possible once logs accumulate (Phase 2, deferred).

**Programming as theory building.** Naur’s thesis frames provenance recall as the product problem: the goal is not merely to find similar text but to reconstitute the theory that produced the code (Naur, 1985). In agentic settings, that theory lives disproportionately in the *intent* trace—human prompts and the human–agent dialogue that fixed constraints—while deliberation and artifact record how and what, respectively. Joint indexing without a path back to intent recreates theory loss at machine speed.

**Contemporary agent memory systems.** Concurrent 2026 preprint work occupies adjacent space. ACT-Up (arXiv:2606.28045) explores activation- and decay-style episodic memory for LLM agents. MRMS (arXiv:2607.04617) studies multi-representation / multi-source agent memory. E-mem (arXiv:2601.21714) emphasizes episodic capture of code state and artifacts. Our positioning—based on apparent scope from titles and topic areas rather than close internal reading, a limitation we state explicitly—is as follows. ACT-Up and MRMS are “one extension away” from three-trace designs but, so far as public descriptions indicate, do not jointly index intent+deliberation+artifact with a code-graph-aware reinstatement walk validated on real multi-hop provenance questions against a production dogfooded corpus. E-mem captures code state without the deliberation dynamics or human-intent trace that make “why” answerable. This paper’s contribution is therefore joint three-trace indexing, a specified and measured reinstatement algorithm, and a methodological finding about observer effects in self-indexing systems—not a claim of exclusive novelty in episodic agent memory writ large.

## System Design

### Architecture overview

CSR is a single Rust binary (`csr-engine`) that replaces an earlier Python/Docker/Qdrant stack. At evaluation time the corpus contained 71,762 chunks across 528 conversations, 2,449 stored reflections, 3,967 code nodes, 13,908 code edges, and 1,256 episode anchors. Embeddings are local (MiniLM, 384 dimensions via FastEmbed); search uses HNSW (`hnsw_rs`, <1 ms p95 for unconstrained queries); persistence is SQLite (`rusqlite`). AST analysis via ast-grep populates `code_nodes` / `code_edges`, linking conversations to files and functions they touched. A `code_evolution` table records the session↔file timeline used both for graph spreading and for ground-truth construction in evaluation. The system exposes fourteen MCP tools and six Claude Code hooks (SessionStart, UserPromptSubmit, PostToolUse, Stop, PreCompact, SessionEnd) that capture sessions continuously.

### Saga schema (Phase 1 data model)

A *saga* is the joint structure over one unit of work:

1. **Intent** — human prompts and human-authored dialogue that state goals and constraints.
2. **Deliberation** — agent reasoning, tool-call traces, dead ends, and sidechain/subagent chatter.
3. **Artifact** — resulting code changes, tracked file-by-file and session-by-session in the code graph.

Phase 1 production changes make saga structure explicit in storage:

- **`chunks.seq`** — import order within a conversation (aligned with the existing chunk index that feeds UUIDv5 chunk ids). Enables future chunk-level temporal reinstatement; not yet used in scoring.
- **`chunks.is_sidechain`** — true if any source message has the JSONL `isSidechain` flag *or* the conversation id starts with `agent-`. Over-labeling was deliberate: live Claude Code places subagent transcripts in separate `agent-*.jsonl` files, so inline `isSidechain` flags were effectively absent. The `agent-*` prefix rule labeled 10,907 of 33,015 backfilled chunks. Labeling is present; sidechain-aware rerank weights are deferred.
- **`retrieval_events` for MCP** — MCP-path searches are logged with `hook_phase='mcp_search'` and sentinel `session_id='mcp'`. Previously only the prompt-submit hook path logged retrieval, rendering the dominant MCP recall path invisible to any future frequency/recency mechanism. Logging unblocks Phase 2 learned decay; it does not implement it.

Deferred non-goals (explicitly not claimed as done): learned decay weighting; chunk-level temporal reinstatement via `seq`; SessionStart saga narration UX; sidechain-aware scoring weights.

### Reinstatement recall (`csr_why`)

The algorithm implements five steps. Defaults: \(k=10\) (max 50), seeds \(=3\), `blend_query_weight=0.65`, `graph_boost=1.10`, `graph_cap_per_seed=6`, `min_score=0.20`. No LLM participates in the retrieval path—only vector and graph arithmetic. Warm latency target is <100 ms; measured warm latency is 8 ms in production.

**Step 1 — Seed retrieval.** Compute top-3 nearest neighbors over the query against chunks and stored reflections merged by cosine similarity. This is exactly the current one-shot baseline used by `reflect_on_past` (minus presentation formatting).

**Step 2 — Context blend (second hop).** For each seed with vector \(s\), form a blended query
\[
q' = \mathrm{renorm}(0.65\, q + 0.35\, s)
\]
and re-run kNN with \(q'\). This is the TCM-style reinstatement step: search from the context associated with the seed memory, not only from the literal cue.

**Step 3 — Code-graph spreading activation.** From each seed’s session, look up files touched in `code_evolution`, find other sessions that touched the same files, and pull the best-matching chunk (cosine vs. the *original* query) from each neighbor session, with a small activation boost (`graph_boost=1.10`). Cap at `graph_cap_per_seed=6` to limit over-spreading.

**Step 4 — Episode chain hop.** From the seed session’s episode reflection, follow one step of `prev_episode_id` (chronological chain of session summaries) and pull the immediately prior episode when present.

**Step 5 — Fuse, rerank, dedupe, cap.** Merge channels, deduplicate, apply `min_score`, rerank the fused pool under a provenance-aware policy (Phase 1.5, below), and return at most \(k\) results as grouped evidence chains citing conversation ids.

### Provenance-aware rerank (Phase 1.5)

The fused pool is wide (`min_score=0.20`, ~46 candidates in practice), so rank order inside it matters. Phase 1.5 applies CSR’s existing rerank machinery to the pool under a dedicated `Provenance` policy, tuned by frozen-corpus evidence rather than by porting `reflect_on_past`’s weights wholesale. Two planned transfers were *rejected* by per-query evidence: demoting tool-mechanic chunks evicted a ground-truth session (edit-heavy chunks are provenance *evidence*, not noise, in this task), and a flat user-authority boost promoted weakly relevant user chunks and user-role compaction summaries over strong evidence. Two defenses were added instead: (1) a **query-echo penalty** (−0.35) on chunks quoting the query near-verbatim—a session that *asks* a question is not the session that *answered* it—paired with echo-aware seed selection (2× seed over-fetch, non-echo hits preferred as walk seeds), and (2) classifying compaction summaries (“This session is being continued from a previous conversation…”) as scaffold text. Without echo-aware seeding, a re-asked question drowns the walk in its own prior askings: in a live test, the reinstatement pool for a repeated query collapsed to three self-referential conversations; with it, five conversations surfaced with the answer-bearing chunk ranked first.

**Implementation note on scoring.** Per-conversation scoring uses exact cosine over that conversation’s stored chunk embeddings (conversations average ~136 chunks—microseconds), *not* a filtered HNSW search. Tiny allowed-id-set filters pathologically escalate `hnsw_rs` toward near-full-index scans; exact scan over a single conversation is both simpler and faster.

### Baseline for comparison

Arm A (baseline) is one-shot kNN over chunks+reflections—the production `reflect_on_past` retrieval core. Arm B is the five-step walk above. Both share the same embedding space and storage; only the multi-hop walk differs.

## Evaluation

### Method

We evaluate *provenance recall*: multi-hop questions of the form “why does the code look like this?”, each paired with a ground-truth (GT) target file. GT is the set of sessions in `code_evolution` that historically touched that file. Primary metric **M1** is summed GT-session coverage across queries at equal budget \(k=10\). **M2** is distinct-conversation diversity in the top-10 (to check that gains are not mere result spraying). **M3** is qualitative side-by-side judging (better / tie / worse per query) plus a junk-rate estimate for B-only results.

**Pre-registered gates** (set before running):

| Gate | Criterion |
|------|-----------|
| **PROCEED** | B ≥ A + 25% summed GT coverage **and** B better on ≥50% of queries **and** junk rate <30% |
| **ITERATE** | Mixed results; allow one weight retune and rerun |
| **KILL** | B ≤ A; write up honestly, thesis dies cheaply |

**Disclosed circularity.** Arm B’s graph-spread step queries the same `code_evolution` table used to define GT, but only via seed sessions found by search: a bad seed yields no graph gain. The design is therefore not circular by construction, but shared plumbing could still inflate graph-channel hits. We therefore require mechanism attribution and M3, not M1 alone.

**Corpus.** Live CSR development history (dogfooding). Phase 0 spike: morning of 2026-07-15, throwaway read-only binary against 71,762-chunk DB, 12 hand-written queries. Phase 1 production: evening of the same day, `csr-engine eval --provenance`, 20-query suite (superset of the 12), after a full day of development sessions had been imported.

### Phase 0 spike results

**Gate: PASSED.**

| Metric | Arm A (kNN) | Arm B (reinstatement) |
|--------|-------------|------------------------|
| Summed GT coverage (k=10) | 15 | 23 |
| Relative lift | — | **+53%** (gate +25%) |
| Per-query outcomes | — | B better 7/12, tie 5/12, worse 0/12 |
| Diversity (distinct conv. in top-10, summed) | 75 | 77 |

Mechanism attribution of B’s GT-hit result lines:

| Channel | GT hits |
|---------|---------|
| Context blend | 24 |
| Graph spread | 9 |
| Episode chain | 0 |

The majority of the win is from the pure-semantics blend hop, not the potentially shared-plumbing graph walk—the key quantitative evidence against circularity as the sole explanation of M1.

**Qualitative highlight (Q3: “why is the integrity check cached”).** Arm A’s entire top-10 was subagent/tool-mechanic chunks with **zero** GT sessions represented. Arm B’s blend hop surfaced the human-origin conversation (`7eccb720`) where caching was decided and discussed with the user—the failure mode the thesis predicts: one-shot similarity drowns in deliberation chatter while reinstatement digs back toward intent. B-only junk rate was eyeballed under 20%, mostly looser-but-relevant graph pulls rather than noise.

### Phase 1 production results

The same walk was reimplemented as engine code and registered as the 14th MCP tool (`csr_why`). Definition-of-Done checks passed: migrations idempotent; backfill processed 732 candidate files (182 missing/skipped, reported not silently dropped); 8 ms warm latency against a 100 ms budget; well-formed evidence chains.

| Run | A | B | Notes |
|-----|---|---|--------|
| Production `eval --provenance` (20 queries) | 15 | 18 | First production path |
| Immediate rerun via original spike code path | 15 | 19 | Faithfulness check |

Production reimplementation is faithful to the spike algorithm (18–19 vs. spike’s 23 on a larger suite under a dirtier corpus), not a regression to a weaker walk.

### Using the discrepancy as evidence

Spike B=23 (clean, pre-development-session) versus production B=18/19 (post same-day import) is not treated as measurement noise to hide. It is primary evidence for the observer-effect finding developed in Discussion: between morning and evening, transcripts of the development session that *discussed and quoted the eval queries and spike output* were imported by always-on hooks. Self-referential chunks textually outrank true origin conversations for the queries that discuss them. Concretely, the acceptance query about integrity-check caching no longer directly surfaces `7eccb720` as it did in the morning spike.

A frozen snapshot (`eval-frozen-2026-07-15.db`, 4.2 GB, 73,342 chunks) was taken, but even that snapshot includes same-day contamination from the spike-discussion session. Morning pre-development numbers remain the cleaner baseline; future protocol should freeze from a session-zero backup before any evaluation dialogue begins. The necessity of freezing was confirmed directly: across four otherwise-identical live-corpus eval runs during Phase 1.5 development, the chunk count drifted from 73,846 to 73,882 as the session’s own transcripts imported, and per-query numbers changed between invocations.

### Phase 1.5 frozen-corpus results

Both binaries (pre- and post-rerank) were evaluated against clones of the frozen snapshot. Aggregate coverage held—A=15, B=18 before and after—while the distribution moved where the thesis predicts: Q3, the acceptance-failure query (“why is the integrity check cached”) whose ground-truth session had been displaced by contamination, recovered from 0 to 1 GT sessions; Q12 improved 1 → 2; Q5 and Q10 each ceded one hit (2 → 1, 4 → 3). The rerank is therefore best read as a *contamination repair* mechanism, not a coverage amplifier: it restores origin conversations on the queries where self-echo displaced them, at the cost of marginal hits elsewhere.

A known remaining gap: for decisions predating the current corpus era (e.g., a major stack replacement), asker sessions still win the top group over the true origin conversation. Chunk-level echo demotion is insufficient when *entire conversations* consist of re-askings; conversation-level echo exclusion and population of a `supersedes` relation are the queued responses.

## Discussion

### What the results support

On this corpus and task, reinstatement recall materially outperforms one-shot kNN for provenance coverage at equal budget. The +53% spike lift cleared a pre-registered +25% gate with zero per-query losses, equal diversity (M2), and qualitative origin-rescue on the canonical failure case (M3). Mechanism attribution places most GT hits in the TCM-style blend channel, supporting the claim that *context reinstatement*, not merely graph co-occurrence, drives the effect. Production DoD confirms the walk can ship as a sub-100 ms, LLM-free MCP tool with honest offline eval.

This is a machine-assisted answer to Naur’s theory-loss problem for agentic coding: when theory lives in the intent trace of a saga, retrieval that only matches surface similarity loses the theory; retrieval that reinhabits seed context and spreads through artifact links can recover it (Naur, 1985; Howard & Kahana, 2002).

### Observer effect / corpus self-contamination

The most important methodological contribution is not the coverage table but the observation that **measuring a self-indexing memory system changes the system**. CSR’s hooks import the evaluation session while the evaluation is designed and discussed. Near-verbatim restatements of queries and spike dumps become high-similarity competitors to origin conversations—exactly the failure mode one-shot retrieval already suffers, now amplified by the experimenters themselves.

Two consequences follow:

1. **Method.** Any eval of self-recording agent memory must run against a **frozen corpus snapshot** taken *before* the evaluation session can be captured and re-imported. Same-day “freeze after discussion” is insufficient. Prefer a session-zero backup. Report both clean and contaminated numbers when contamination is unavoidable; use the gap as a diagnostic, not as a reason to discard the clean run.
2. **Design.** Phase 1.5 applied provenance-aware reranking to the reinstatement pool inside `csr_why` and measured it on the frozen corpus (see Evaluation). The hypothesis—that echo demotion specifically counters contamination—was partially confirmed: query-echo defenses restored origin-surfacing on the canonical displaced query (Q3, 0 → 1) while holding aggregate coverage, but chunk-level demotion does not rescue decisions whose re-askings dominate whole conversations. Notably, two rerank heuristics that work in general-purpose search (`reflect_on_past`) were *rejected* by evidence in the provenance task: tool-mechanic chunks are evidence there, not noise, and flat user-authority boosts promote weak user chatter over strong evidence. Rerank policies are task-relative.

This observer effect is a Heisenberg-like issue for a class of systems, not a CSR-only bug: any agent that indexes its own tool use while researchers discuss eval items will inject high-similarity confounds.

### Limitations

**(a) Scale and design.** Evaluation uses 12–20 hand-written queries on a single system, single corpus, with single-annotator M3—no inter-rater reliability, no external dataset, no multi-organization coding history.

**(b) GT and graph plumbing.** GT is built from `code_evolution`, which Arm B also uses in step 3. Circularity is mitigated by seed dependence and by blend-channel dominance in attribution, but not eliminated.

**(c) Observer effect partially mitigated, not solved.** Frozen snapshots and Phase 1.5 echo defenses repair the measured displacement cases but do not rescue origin conversations for decisions whose re-askings dominate entire conversations; conversation-level exclusion and `supersedes` chains remain open work.

**(d) Episode-chain null result.** The episode hop contributed zero GT hits so far, attributed to sparse/young `prev_episode_id` chains. Strength is expected to grow as chained episode summaries accrue; currently unproven.

**(e) Generalizability.** Validation is maximally dogfooded: CSR indexing its own development history is a favorable case. Results are unproven on other agentic coding corpora or non-coding agent domains.

**(f) Related-work positioning.** Comparisons to ACT-Up, MRMS, and E-mem are based on apparent scope from titles/topic areas of concurrent 2026 preprints, not close reading of their internals; overclaiming familiarity would be dishonest.

**(g) Incomplete three-source modeling.** CMR motivates source-factored reinstatement of intent vs. deliberation vs. artifact (Polyn et al., 2009). Phase 1 labels sidechains and stores sequence but still uses a single fused context blend and does not yet weight by `is_sidechain` or `seq`.

### Deferred work and theoretical next steps

Learned decay (Anderson & Schooler, 1991) is theoretically motivated and now *unblocked* by MCP retrieval logging, but blocked on log accumulation (Phase 2). Chunk-level temporal reinstatement using `seq`, sidechain-aware scoring, and SessionStart saga narration remain non-goals of this phase. With Phase 1.5 shipped, the immediate product next steps are conversation-level echo exclusion and `supersedes` population; external validation is the immediate scientific next step.

## Conclusion

We introduced *three-trace sagas*—joint episodic structure over intent, deliberation, and artifact in agentic software construction—and *reinstatement recall*, a five-step, LLM-free retrieval algorithm that seeds with kNN, blends query and seed context (TCM analogue), spreads over a session–file code graph, hops episode chains, and fuses results under a provenance-aware rerank. On CSR’s own development corpus, a pre-registered spike showed +53% summed GT-session coverage at k=10 versus one-shot kNN, with mechanism attribution locating most gains in semantic context blend rather than graph walk. Production reimplementation as `csr_why` met latency and integration DoD under partial same-day self-contamination (15 → 18/19), and a frozen-corpus rerank pass repaired the contamination-displaced acceptance query (0 → 1) while holding aggregate coverage.

**Proven on this corpus:** reinstatement beats flat kNN for multi-hop provenance questions under dogfooded conditions, at sub-100 ms warm cost, without an LLM in the loop; query-echo demotion repairs measured contamination displacement without sacrificing coverage.

**Not yet proven:** learned usage-weighted decay; sidechain-weighted scoring; chunk-sequence temporal reinstatement; episode-chain value; generalization beyond self-indexing CSR history; origin recovery for decisions whose re-askings dominate whole conversations.

**Immediate next steps:** conversation-level echo exclusion and `supersedes` population; enforce session-zero frozen snapshots for all future evals; accumulate MCP retrieval logs for Phase 2 learned decay; validate on external agentic coding corpora.

For agentic construction, Naur’s theory is no longer only in human heads—it is stranded across sagas. Systems that index only similar text will keep losing it. Systems that reinstate the encoding context of the work that produced the code have a chance to keep it.

## References

Anderson, J. R., & Schooler, L. J. (1991). Reflections of the environment in memory. *Psychological Science, 2*(6), 396–408.

Howard, M. W., & Kahana, M. J. (2002). A distributed representation of temporal context. *Journal of Mathematical Psychology, 46*(3), 269–299.

Naur, P. (1985). Programming as theory building. *Microprocessing and Microprogramming, 15*(5), 253–261.

Polyn, S. M., Norman, K. A., & Kahana, M. J. (2009). A context maintenance and retrieval model of organizational processes in free recall. *Psychological Review, 116*(1), 129–156.

ACT-Up. (2026). Activation and decay-style episodic memory for LLM agents. *arXiv preprint* arXiv:2606.28045.

E-mem. (2026). Episodic memory for code state and artifacts in agent systems. *arXiv preprint* arXiv:2601.21714.

MRMS. (2026). Multi-representation multi-source memory for agents. *arXiv preprint* arXiv:2607.04617.
