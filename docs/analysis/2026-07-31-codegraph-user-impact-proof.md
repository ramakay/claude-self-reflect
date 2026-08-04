# WP3 — User-impact proof: what CSR can answer now that it could not before

**Date:** 2026-07-31 · **Branch:** `feat/codegraph-truth-pass` · **Companion:** `.plans/2026-07-31-codegraph-shipping-plan.md` (receipts R1–R10)

### Read this first — what "before" and "after" mean here

Two binaries are in play and the difference is the whole point:

| Layer | What ran | Status |
|---|---|---|
| **BEFORE** | MCP tools `mcp__claude-self-reflect__*`, served by the **installed 9.3.1 binary** | This is what a user has today. It has similarity search, the co-edit ledger, and the AST graph — but it renders provenance as a single projected `first_conv_id`/`last_conv_id` per symbol. |
| **AFTER** | Branch binary `csr-engine/target/release/csr-engine` + `sqlite3` against `code_node_attribution` / `code_nodes.repo_root` / `code_evolution.repo_root` | Two independent evidence channels per symbol, git-toplevel repo identity, and a structural coverage gate. **Not yet rendered by the MCP surface** — the SQL below is what the shipped renderer will show once the 9.3.1 binary is replaced. |

Every number below is either from the plan's receipts table (cited as R1–R10) or from a query I ran against the live DB `~/.claude-self-reflect/csr-engine.db` on 2026-07-31 (read-only). Where a live number drifts from a receipt, both are shown.

---

## TL;DR

Before this branch, when you asked "who wrote this function and why," CSR answered with **one conversation id projected across every symbol in the file** — 500 of 539 indexed files carried exactly one distinct `first_conv_id` (live count; R2 measured 499/532, 50.74% agreement against the actual edit events). After this branch, each symbol carries up to two independently-derived receipts — a transcript session and a git commit — that are stored separately, never merged, and rendered as a **labeled disagreement** when they conflict.

The second change is that CSR now knows when it doesn't know: 1,185 nodes (17.0%) are marked `unattributed` rather than given a plausible-looking conversation id, `csr_search_by_file` returns `indexed='false'` for files the AST layer never saw, and the new `structural_file_coverage` line reports 400/1737 = 23.0% out loud in every eval run.

The third change is repo identity: 2,837 CSR symbols were split across two session-cwd labels (`claude-self-reflect-csr-engine` 1,583 / `claude-self-reflect` 1,254) and are now unified under one git-toplevel `repo_root`, which is what makes cross-subsystem history queries possible at all.

---

## Angle 1 — "How has Rust usage evolved in this codebase?"

### (A) BEFORE — similarity-only retrieval

`csr_reflect_on_past("How has Rust usage evolved in this codebase?")`, 13ms, top score 0.607:

```
rank 1  0.607  subagents  today   "79  "io", 80 "csv", ... 86 /// Rust's own namespace
                                   segments — never a project dependency.
                                   const RUST_BUILTIN_NAMESPACES: &[&str] = ..."   agent-a2cffefc
rank 2  0.607  subagents  today   (byte-identical duplicate of rank 1)             agent-ad886d4c
rank 3  0.566  subagents  today   "SERVICE_SOURCE, SupportLang::Rust,"             agent-a7694125
rank 4  0.565  subagents  3w ago  linfa-logistic crate docs (a different project)  agent-a8b662ce
rank 5  0.555  csr-engine 2w ago  memory file: rust_engine_phase_patterns
rank 6  0.541  csr-engine 3w ago  "rustc 1.95.0 (59807616e 2026-04-14)"
```

The query says *evolved*. The retriever matched on the token "Rust". Four of six hits are subagent chunks that merely *contain* Rust source, two of them byte-identical duplicates, one is documentation for an unrelated ML crate. There is no timeline, no ordering, no sense of which subsystem came first. The single genuinely useful hit (rank 5, the archived phase-history memory) is ranked below a duplicate pair.

**What grep/git alone gives:** `git log -- csr-engine/src` returns 158 commits, oldest `624e722 2026-05-02 v8.0: Rust engine, docs site, single binary distribution`. That single squash commit is the origin of most of the tree — git can tell you *when the squash landed*, not what happened during the six months of work inside it.

### (B) AFTER — co-edit ledger + repo identity + two-channel attribution

**Volume over time** (`code_evolution`, filtered to `repo_root = .../claude-self-reflect`, `.rs` files, functions counted through `json_each(functions_added)`):

| month | fn-add events | distinct functions | edits | sessions | files |
|---|---|---|---|---|---|
| 2026-05 | 47 | 47 | 21 | 3 | 12 |
| 2026-06 | 308 | 289 | 126 | 8 | 33 |
| 2026-07 | 593 | 559 | 493 | 20 | 54 |

**The phase arc** — first and last observed edit per subsystem, which is the actual answer to "how did it evolve":

| subsystem | first touch | last touch | edits | sessions |
|---|---|---|---|---|
| storage | 2026-05-11 | 2026-07-31 | 63 | 11 |
| hooks | 2026-05-11 | 2026-07-30 | 158 | 16 |
| enrichment | 2026-05-11 | 2026-07-27 | 29 | 4 |
| search / rerank | 2026-05-15 | 2026-07-27 | 65 | 8 |
| import | 2026-06-10 | 2026-07-31 | 31 | 9 |
| eval | 2026-06-11 | 2026-07-31 | 68 | 8 |
| mcp | 2026-06-27 | 2026-07-30 | 14 | 5 |

Storage and hooks first, then search, then import, then eval/provenance, with the MCP tool surface touched last and least (14 edits — it is a thin façade over the engine, and the ledger shows that structurally). This is the arc the CLAUDE.md narrates from memory; here it is derived from evidence.

**Which sessions did it** — top Rust sessions by distinct functions introduced, each id a live receipt:

| session | day | fns | files | what it touched |
|---|---|---|---|---|
| `c011dc7b` | 2026-07-30 | 354 | 10 | storage/migrations, storage/codegraph, extraction/{codegraph,repo_scan,manifest,resolver}, eval/codegraph |
| `69a05719` | 2026-06-11 | 77 | 8 | eval/continuity, provenance, storage/{mod,queries}, search/rerank, import, ledger, governor |
| `70690eeb` | 2026-06-27 | 67 | 10 | storage/codegraph, extraction/{codegraph,resolver}, search/code_rank, mcp/{tools,mod}, hooks/{post_tool_use,prompt_submit} |
| `fc16f91d` | 2026-06-11 | 54 | 7 | hooks/{session_start,stop,session_end}, extraction/provenance, storage |
| `f8808597` | 2026-07-27 | 47 | 10 | storage/{queries,mod}, format |
| `efc96e9e` | 2026-06-01 | 43 | 6 | telemetry/{mod,parser,aggregator,render,tui}, status |
| `cce6e815` | 2026-07-31 | 38 | 7 | examples/codegraph_ablation, storage/migrations, extraction/repo_root, storage/codegraph |
| `8b266e91` | 2026-05-17 | 26 | 4 | hooks/{stop,install,session_briefing,session_start} |
| `55715673` | 2026-07-08 | 20 | 1 | hooks/intent |
| `bb1688ad` | 2026-07-15 | 19 | 2 | search/{reinstatement,rerank} |

**The receipt that only this branch can produce.** Session `70690eeb` shows up above with 67 functions across the codegraph subsystem. Its raw transcript is gone:

```
$ find ~/.claude/projects -name '*70690eeb*'
(no output)
$ sqlite3 csr-engine.db "SELECT COUNT(*) FROM chunks WHERE conversation_id LIKE '70690eeb%'"
196
```

The JSONL was deleted. 196 embedded chunks and 126 transcript-channel attribution rows survive it. Grep against `~/.claude/projects` returns nothing for that session; CSR still names it as the introducer of `extract_graph_fragment_for_file`, corroborated independently by git:

```
name        = extract_graph_fragment_for_file
file        = csr-engine/src/extraction/codegraph.rs
channel     = transcript   source = 70690eeb-8942-…   ts = 2026-06-27 05:28:39   evidence = coedit_event
channel     = git          source = a190db66…         ts = 2026-07-07T20:44:13   evidence = git_log_L
                                    (a190db66 = "feat(codegraph): add conversation-provenance code graph (v9.4)")
```

Two channels, ten days apart, agreeing on *what* while disagreeing on *when* — because the session did the work on 06-27 and the commit squashed on 07-07. Both values are stored; neither overwrites the other.

**Repo identity (the H8 fix, R4).** Before: `code_nodes.project` split one repo into two labels — `claude-self-reflect-csr-engine` (1,583 nodes) and `claude-self-reflect` (1,254) — because `project` is the *session cwd*, not the repository. Every "how did this repo evolve" query silently answered about half the repo. After: both carry `repo_root = /Users/ramakrishnanannaswamy/projects/claude-self-reflect` (2,837 nodes) while `project` is preserved as its own signal. The `code_evolution` ledger is likewise now repo-keyed (894 CSR rows; 1,027 rows still null where the file was outside any git tree).

**Language census across all indexed repos** — the honest denominator for "how much Rust": rust 2,890 · tsx 1,571 · typescript 1,479 · javascript 826 · python 219, of 6,985 nodes. Rust kinds: function 2,657 / type 167 / module 66.

---

## Angle 2 — "csr_why — how did that come up?"

### (A) BEFORE — similarity-only

`csr_reflect_on_past("csr_why tool how did it come up")`, 7ms, top score 0.651:

```
rank 1  0.651  csr-engine  6d ago  "t/csr-engine"                      73f0fb7d
rank 2  0.641  csr-engine  today   "…/03131be8-….jsonl:"file_path":".../resolver.rs"
                                    === which tool_use names wrote it ==="   243d3dc8
rank 3  0.587  subagents   1d ago  "[reflection] 5 specific code-location queries…"
rank 4  0.576  csr-engine  3w ago  "i might be confused on 1. what would the csr-engine detect?"
rank 5  0.572  csr-engine  1mo ago "=== installed PostToolUse hook command ==="   70690eeb
```

The top-ranked result at 0.651 is the twelve-character fragment `t/csr-engine`. Rank 4 is a user asking a confused question. Nothing here answers the question; the embedding matched the substring `csr-engine` and the word "how".

**What grep gives:** `grep -rn "csr_why" src` returns three lines — the tool registration at `src/mcp/mod.rs:642` and two comments in `src/search/rerank.rs`. The implementation is `why()` at `src/mcp/tools.rs:678` and the engine is `reinstate()` at `src/search/reinstatement.rs:302`. Grep can find the code. It cannot tell you the tool exists because a ranking hypothesis was tested, partly failed, and got relocated into a provenance tool.

### (B) AFTER — full stack

**`csr_why` on itself** (this runs on the shipped 9.3.1 binary — the reinstatement walk already works; what's new is the receipts underneath). Query: *"why does the csr_why provenance tool exist — what problem did reinstatement recall solve"*, 6 conversations:

```
conv 513b2781  0.871  3d ago  "…cause (echo defenses inverting) and a known fix (route lookups
                               to the receipt join, keep the walk for why-questions). The same
                               defenses that lose Gate M are the ones that *won* the original
                               provenance eval. It's a routing bug, not a concept failure…
                               So: reinstatement-as-general-retrieval died.
                               Reinstatement-as-provenance-tool keeps its original scoped result."
conv fc16f91d  0.859  1mo ago "refactor(extraction): centralize self-reference filtering in
                               provenance module … create mode 100644 src/extraction/provenance.rs"
conv 758a3f25  0.858  3d ago  "+47% on a TypeScript/marketing corpus replicates the +53% on the
                               Rust/systems corpus — evidence the advantage is not an artifact
                               of CSR describing itself."
conv agent-afb893dd 0.767 2w  the second-corpus replication (613 code_evolution rows vs CSR's 180)
conv 73f0fb7d  0.745  6d ago  the paper section carrying A=15, B=22 (+47%)

seeds -> graph/episode reach: 0 seed(s), 0 graph hop(s), 0 episode hop(s)
```

That is the actual origin story, in order: a ranking idea (+53% on Rust, replicated +47% on TypeScript), a general-retrieval ambition that died, and a deliberate narrowing to "reinstatement-as-provenance-tool." Note the last line — the walk reports **0 graph hops and 0 episode hops**. It found these by blending, and it says so rather than implying structural evidence it didn't use.

**`csr_code_graph(symbol="reinstate")`** — the shipped renderer:

```
out calls → best_chunk_for_conv, episode_prev_session, select_seed_indexes,
            blend, rerank_pool, push_candidate, clean_excerpt   (all last_conv='bb1688ad-…')
in  calls ← walk_reinstatement  (examples/saga_relitigation.rs, last_conv='f8808597-…')
in  defines ← module src/search/reinstatement.rs                (last_conv='bb1688ad-…')
```

Structurally correct — and *every* symbol in the file reports the same `bb1688ad`. That is R2's projection in the wild: 40 of 40 nodes in `reinstatement.rs` carry `first_conv_id = last_conv_id = bb1688ad`.

**What the branch adds.** The same 40 symbols, split by evidence:

| git commit | symbols | subject |
|---|---|---|
| `33559079` (2026-07-15 12:01) | 24 | `feat(saga): WS2 — reinstatement recall module, csr_why MCP tool (14th), eval --provenance gate` |
| `7dd95a7b` (2026-07-15 13:24) | 14 | `feat(saga): Phase 1.5 — provenance-aware rerank in reinstatement pool` |
| `4ffb3052` (2026-07-18 19:16) | 1 | `feat(ratification): shadow signal on reinstatement evidence — logged, never ranked` |

And the transcript channel independently confirms the second wave — session `bb1688ad` at 20:08–20:20 on the same day introduced exactly the echo-defense functions that `7dd95a7b` committed:

```
is_query_echo                                  git 7dd95a7b 13:24  |  transcript bb1688ad 20:20
rerank_pool                                    git 7dd95a7b 13:24  |  transcript bb1688ad 20:08
rerank_pool_demotes_verbatim_query_echo        git 7dd95a7b 13:24  |  transcript bb1688ad 20:17
rerank_pool_demotes_contaminated_echo_below_origin  git 7dd95a7b  |  transcript bb1688ad 20:08
seed_selection_falls_back_to_echoes_when_starved    git 7dd95a7b  |  transcript bb1688ad 20:20
select_seed_indexes                            git 7dd95a7b 13:24  |  transcript bb1688ad 20:20
CandidateDetail (type)                         git 7dd95a7b 13:24  |  transcript bb1688ad 20:08
ratification_is_shadow_only_does_not_affect_order   git 4ffb3052 (2026-07-18), transcript: none
```

So the shipped answer is "one session wrote this file." The branch answer is "the module and the `csr_why` tool landed together at 12:01; ninety minutes later a second pass added the echo defenses — the ones conv `513b2781` later identified as inverting on Gate M; three days after that, ratification was wired in as shadow-only." The one function with a git receipt and no transcript receipt (`ratification_is_shadow_only…`) is shown with a single channel, not backfilled with a guess.

**The disagreement case, rendered not merged** — `insert_chunk`, the R8 example:

```
insert_chunk  src/storage/queries.rs   git 624e7229  2026-05-02T13:06:00  git_log_L
insert_chunk  src/storage/queries.rs   transcript f8808597  2026-07-27 16:00:44  coedit_event
```

86 days apart. Git's answer is the v8.0 squash — technically true and useless; the transcript's answer is the session that actually worked on it. Both are surfaced with their channel label. The old behavior would have picked one and presented it as *the* introduction.

---

## Angle 3 — "Memory decay — where is it implemented, which functions, how, why?"

This is the angle where the honest answer is partly "I don't have it," and that is the interesting result.

### (A) BEFORE — similarity-only

`csr_search_by_concept("memory decay half-life scoring")`, top 0.624 — and here similarity actually performs well:

```
rank 1  0.624  DecayConfig::for_injection() { decay_weight: 0.5, base_half_life_days: 30.0 }
               DecayConfig::for_search()    { decay_weight: 0.3, base_half_life_days: 90.0 }
rank 2  0.595  time_factor = 2.0.powf(-age_days / effective_half_life)
rank 3  0.584  apply_tad(...) — "memories that helped in successful sessions persist longer"
rank 4  0.572  DEFAULT_DECAY_WEIGHT = 0.3; DEFAULT_SCALE_DAYS = 90.0; apply_decay(...)
rank 5  0.564  DecayConfig struct + Default impl
```

All five hits are from a **single conversation, `agent-a7d3813ac86c386c3` (2026-07-15)** — a subagent that *read* the file two months after it was written. Similarity gave you the source code faithfully. What it gave you as provenance is a reader, presented with the same shape and confidence as an author. Nothing in the output distinguishes "this session wrote it" from "this session looked at it."

**What grep gives:** `src/search/decay.rs` — `apply_decay` (l.16), `DecayConfig` (l.39) with `for_injection` (30d/0.5) and `for_search` (90d/0.3), `apply_tad` (l.93), `compute_reinforcement` (l.114), plus a second, unrelated decay at `src/hooks/prompt_submit.rs:554` — `EPISODE_RECENCY_HALF_LIFE_DAYS: f32 = 7.0`. Grep finds both. Grep cannot tell you they are different mechanisms with different owners.

### (B) AFTER — full stack, including the abstention

**The AST/attribution layer abstains, visibly:**

```
sqlite3> SELECT COUNT(*) FROM code_nodes WHERE file LIKE '%decay%';
(0 rows)
sqlite3> SELECT * FROM code_nodes WHERE name IN ('apply_decay','apply_tad','compute_reinforcement');
(0 rows)
sqlite3> SELECT COUNT(*) FROM code_evolution WHERE file_path LIKE '%search/decay.rs';
0
```

`csr_search_by_file("…/csr-engine/src/search/decay.rs")`:

```xml
<file_search indexed='false'>
  <message>No conversations found analyzing …/src/search/decay.rs</message>
</file_search>
```

`indexed='false'` is the feature. `decay.rs` is a supported `.rs` file in an indexed repo that the AST layer has never seen, because the corpus is **edit-observed by design**: nodes enter the graph when a session edits the file. Nobody has edited `decay.rs` since it was written. The system does not invent a `first_conv_id` for it. It says it has nothing.

The new coverage gate quantifies exactly this, live, on every eval run:

```
structural_file_coverage (informational, H8 innovation, not gated):
  overall = 400/1737 (23.0%); repo_roots measured=7, skipped(enumeration failed)=0
  claude-self-reflect      66/108  (61.1%)
  anukriti-command-center  92/169  (54.4%)
  procsolve-website        67/172  (39.0%)
  anukriti                137/610  (22.5%)
  anukriti-ota-rls-template 2/537  (0.4%)
```

`decay.rs` is one of the 42 CSR files in the missing 39%. Before this branch that gap was invisible — you could not distinguish "CSR has no evidence" from "CSR wasn't asked."

**Git still answers, coarsely and honestly:**

```
$ git log --follow -- csr-engine/src/search/decay.rs
624e722  2026-05-02  v8.0: Rust engine, docs site, single binary distribution
$ git log -L '93,112:csr-engine/src/search/decay.rs' --reverse --format='%h %ad %s'
624e722  2026-05-02  v8.0: …          # apply_tad
$ git log -L '16,34:csr-engine/src/search/decay.rs' --reverse --format='%h %ad %s'
624e722  2026-05-02  v8.0: …          # apply_decay
```

One commit, ever. `decay.rs` shipped complete in the v8.0 squash and has not been touched since — which is itself the answer to "how has it evolved": it hasn't. (Trap per R8: `-1` before `--reverse` returns the *newest* commit; the queries above deliberately omit `-1`.)

**The "why" still comes from conversations.** `csr_why("why 90-day half-life for search decay and 30-day for injection")` returns 2 conversations, and reports `0 seed(s), 0 graph hop(s), 0 episode hop(s)` — no structural evidence available, consistent with the abstention above. The substantive hit is the rationale carried in the code's own doc comments, plus one genuinely new fact from `f8808597` (2026-07-29):

```
f8808597  0.588  "159: let tad_config = decay::DecayConfig::for_search();
                  168/206: decay::apply_tad(r.score, &ts, &now, events, &tad_config)
                  272: decay::apply_decay(0.45, &ts, &now, None, None)  // base 0.45 + decay"
agent-a7d3813a 0.581  "an explicit MCP search/recall does not record which chunks were returned or
                  when. So per-memory re-access history is captured only for hook-time
                  auto-injection, not for user/agent-initiated searches. For a learned-decay
                  feature needing full re-access history across all retrieval surfaces, the MCP
                  path is a logging gap."
```

**The synthesized answer a user gets:** decay is implemented twice, for different jobs.
- `src/search/decay.rs` — score decay. `apply_decay(score, ts, now, weight, scale)` = `score × ((1−w) + w·2^(−age/scale))`, defaults w=0.3, scale=90d. `DecayConfig::for_injection()` = 30d/0.5 (recent context wins at hook time); `DecayConfig::for_search()` = 90d/0.3 (old results stay findable). `apply_tad` layers Temporal Attention Decay on top — `effective_half_life = base × 2^reinforcement` from `retrieval_events`, so memories that helped in successful sessions decay slower and memories from failed sessions decay faster. Provenance: `git:624e7229` only. `transcript: unattributed`. `ast: not indexed`.
- `src/hooks/prompt_submit.rs:554` — `EPISODE_RECENCY_HALF_LIFE_DAYS = 7.0`, `raw × 0.5^(age/7d)`, used for episode-correlation ordering. Different mechanism, different half-life, different subsystem. (Per CLAUDE.md, plan-source decay is a third: mtime-driven.)
- Known limitation, sourced: TAD's reinforcement signal only sees hook-time auto-injection; explicit MCP searches are not logged as retrieval events (`agent-a7d3813a`) — so `apply_tad` is under-fed on the user-initiated path.

Every one of those claims carries either a commit hash, a session id, or an explicit `unattributed` / `not indexed` label. None is a guess dressed as a receipt.

---

## What a Mem0/Zep-class memory system would return

These systems store conversation-derived memory — Mem0 extracts and consolidates natural-language facts with an add/update/delete reconciliation pass; Zep (Graphiti) builds a bi-temporal entity-relationship graph from dialogue turns with validity intervals. Both would handle Angle 2 respectably: "the user built a provenance tool called csr_why after a retrieval experiment showed a +53% lift" is exactly a conversation-derived fact, and Zep's temporal edges would even capture that the claim was later narrowed. For Angle 1 they would return a fluent summary of a Rust rewrite, ordered by when it was *discussed*, and Zep would give an entity graph of CSR → csr-engine → Rust → subsystems. For Angle 3 they would return whatever was said about decay in conversation. The structural gap is not summary quality, it is grounding: neither system parses the repository, so neither has a `code_nodes` row, a span, a `body_hash`, or a git commit to point at. There is no channel that is independent of the transcript, so there is nothing to corroborate against and no way to represent a *disagreement* between what was said and what was committed — the reconciliation step's job is precisely to collapse conflicts into one accepted fact, which is the opposite of the `insert_chunk` rendering above. And because extraction is generative, absence is unrepresentable: asked "who wrote `apply_tad`," a summary memory returns its best conversational match with the same fluency it uses for a fact it actually holds. It has no `indexed='false'`, no `unattributed`, no `400/1737 (23.0%)`. CSR's differentiator is not that it remembers more; on conversational recall these systems are strong. It is that CSR can be *checked* — against the AST, against git — and can therefore afford to say nothing.

---

## Honesty section — what still doesn't work

**The AST channel contributes nothing to ranking.** R6/H1: the structural expansion arm `S_A` is byte-identical to the base `S` on origin-MRR across all 12 mapped queries, and indistinguishable from its own degree-preserving edge-shuffle sham (nDCG Δ +0.0002, 95% CI [−0.013, +0.014]); the channel fired on only 8 of 20 queries. Everything shown in Angles 2 and 3 from `code_nodes`/`code_edges` is an **integrity** layer, not a relevance layer. Per R9 that null is bounded by attribution quality (the seed mapping used the 50.7%-projected `first_conv_id`), which is why a rematch on `code_node_attribution` is queued as WP4 — but as measured today, the honest statement is: AST buys correctness, not better hits.

**Co-edit's ranking contribution is directional, not separable.** R7/H2: +0.0167 MRR (95% CI [0.0000, +0.0500], lower bound exactly zero), +0.0264 nDCG (CI includes zero), n=12/20. Real-looking, not proven at this power.

**1,185 nodes (17.0%) are unattributed** — javascript 578, tsx 354, typescript 123, rust 85, python 45. Most are `module`-kind nodes, which have no meaningful line span for `git log -L` and no name to match against `functions_added`. Coverage overall: git-only 3,435 (49.2%), both channels 2,174 (31.1%), transcript-only 191 (2.7%). Transcript coverage is structurally capped — R3/H5 measured 33.06% overall against a 44.48% ceiling among function+type kinds, with 1,740 const/module rows unattributable by construction.

**The two channels agree less than the headline.** Live: of the 2,174 dual-channel nodes, 1,611 (74.1%) agree within 48h. R8's H6 sample reported 1,103/1,342 = 82.2%; the denominators differ (H6 sampled symbols with clean spans; the live table includes everything backfilled), and I am reporting mine as measured, not reconciling it to the receipt. Roughly a quarter of dual-channel symbols disagree by more than two days — mostly squash artifacts (`624e7229` alone attributes 792 symbols, `4a4729e6` another 415), which is why coarseness is labeled rather than hidden.

**Structural coverage is 23.0% and is not gated.** 400 of 1,737 enumerated supported files. Best repo 61.1% (claude-self-reflect), worst 0.4% (anukriti-ota-rls-template, a template repo nobody edits). This is by design — the corpus is edit-observed — but it means "CSR has no record" is a frequent and legitimate answer, and the gate stays *informational* until the next corpus version rather than being promoted to a bar it would fail.

**The resolution gate fails on purpose.** `csr-engine eval --codegraph --live` on this branch: **11/12 passed**, with `[FAIL] Resolution rate 4499/16007 = 28.1%` against a 70% threshold. R5/H3 established that bar is unreachable — the ceiling is 46.1%, and even that is inflated by generic-name collisions (`path`, `log`, `prepare`). The failing line is kept visible rather than retuned to green. The gates that do pass are the integrity ones: witness closure 98.0% (bar 90), internal binding 85.4% (bar 70), drifted=0, project attribution 0/6985 unscoped, no placeholder leak in injection.

The through-line: every one of those numbers is a thing the system says about itself, out loud, in the same output a user reads. `indexed='false'`, `unattributed`, `unverified:`, `0 graph hop(s)`, `23.0%`, `[FAIL]`. A memory system that can't produce those strings can't be trusted when it does produce an answer — that is the argument, and it is why the abstentions above are printed rather than smoothed.

---

## Correction — 2026-08-03

The claim above that CSR "can therefore afford to say nothing" did not hold for the `csr_quick_check` surface on the day this doc was written. Four probes for events that never happened on this machine all returned `count=1` with a confident preview: Kubernetes/Helm deployment (0.461), SQLite→PostgreSQL migration (0.583), an Elixir Phoenix LiveView rewrite (0.588), a contractor payroll onboarding call (0.456). `format_quick_check` emitted a raw count and score with no relevance interpretation and no floor, so a fabricated topic and a real one were shape-identical to the caller. A repair on `feat/codegraph-truth-pass` adds an abstention floor (0.45), relevance banding shared with the search renderer, and a below-floor response of `<found>false</found>` with the preview withheld. Measurement also found that the fabricated and genuine score distributions **overlap** (fabricated 0.308–0.605 over 8 probes, genuine 0.468–0.816 over 12): no floor separates them cleanly, so matches in the 0.45–0.62 band are labelled `weak` and carry an explicit may-be-spurious warning rather than being suppressed. Abstention on this surface is now implemented, not yet clean.
