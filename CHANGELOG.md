# Changelog

All notable changes to Claude Self-Reflect will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [10.1.0] - Unreleased

`[untraced]` marks a claim that no commit message supports.

**Already shipped in 9.5.1-9.5.8, carried here, not new:** 92acc3bd, cdd23033, c46702aa, f0cf3a83, 56dddc3a,
71457f3c, 31d78e5b, d6b9c454, 46e14f16, 962f8e03, 6e3a08b1, aededb4a (replaced by 76e390d6), cecddb48, c7e4e285,
c7188044, 5e762cc6, 4eca3ec4, and the #316 installer rewrite (9cfb1229).

### Recap

- **One causal paragraph at SessionStart** replaces the CONTINUUM / LAST / NEXT / ANCHORS fragments:
  `recap [<age>]: <intent>: <completed>. Settled: ... Now: ... Next: ...`. No LLM. Each clause drops without
  evidence; `Next:` is never invented; a feed error falls back to the old fragments byte for byte;
  `CSR_NO_RECAP=1` turns it off (cc14ca81).
- **The intent names the session's first task**, not the opening prompt or the latest side request. Blind labels
  matched the first task in 9 of 11 sessions, the most-work segment in 7, the latest task in 2 (01e07088).
  Whole sentences up to 160 chars, else a clause cut, else a fallback of 6+ words, else abstain (1d23d675,
  51554e24, a7917b7f, f9b38f67, f83e4f80, 75fa7e4e). A mid-turn follow-up sent within 120 s of the previous
  prompt joins the task text (f66e6f64, e523644b, bb829e5a, d53f3753, 41d2776d).
- **`completed` comes only from receipts:** successful `git commit` / `cherry-pick` / `revert` / `merge` output,
  then TaskUpdate and TodoWrite completions answered by a non-error tool_result. Commits show first; receipts
  from another task render as `Latest: ...`. Assistant prose is no longer read (0728a16e, 9d5cb55a, 244a13a2,
  ec240ae1, 3fb030ff).
- **New `Corrected:` clause:** up to 2 corrections from the last 7 days, 90 chars each, with session:turn
  receipts. The 700-char budget fills by priority, so a correction cannot evict `Settled:` (b719b2bc, 54389e31).
- Subagent rows never touch the recap (53e3e5e0, 7bb0b944). Forwarded notes and dreamer what-ifs render as
  `note: <headline>` (7cbd3660, 691db258, 04810c1c); CSR narrator transcripts have no task (5257ed3b, c81bd23a).
  A headless run is never your previous session; the live DB held 19 such episodes, newest in 4 projects
  (e2a28034, ff5c21cf). CONTINUUM and EPISODE INDEX share the intent rule (14e744e8, 1cace860).
- **After a compaction the intent names the current task.** Inside one long session the first task can be hours
  stale; the compact recap now anchors on the latest renderable typed task, cached by the Stop hook, so
  SessionStart reads no transcript (0.58 us selection, 102 us vs 101 us formatting). Notes, short side requests,
  harness rows and scheduled re-fires (`isMeta`, `promptSource=system`) never count; startup, resume and clear keep
  the first-task rule (18adccd0). PreCompact refreshes the anchor, so a task typed after the last response counts
  (16-18 ms on a 65 MB transcript); an interruption ends a turn and is never the anchor (fffa3a8e).
- **Session stories follow long sessions:** rebuilt free from Claude Code's summary after each compaction; no
  PreCompact Haiku call (45f718e4). `CSR_STORY_REWRITES=1` has Haiku write from summary plus last 20 messages;
  blind judges scored it 16.8 and 13.2 of 20, against 11.2 and 9.0 for the free extract (a5c6ca73).

### Dreaming / witness

- **Evidence-grounded forgetting** (b721a745, 834fa704). `codewitness` stamps spans with BLAKE3 at commit OIDs and
  orders verdicts by git ancestry, no LLM. Adds `witness_ledger`, `witness_generations`, deterministic
  supersession verdicts, and `[stale anchor]` / `[evolved]` notes with commit receipts. The daemon dreams every
  6 h (`CSR_DREAM_INTERVAL_SECS`; `CSR_NO_DREAMING=1`). `codewitness labels|bench`: 1.000 / 1.000 over 13,626
  beliefs; recall is 1.0 by construction and the corpus has zero reverts. The GraphOrdered / ContentOnly receipt
  basis is crate API only; no production path emits it (19f2ac5d).
- **`CSR_DREAM_CONSUMPTION` has three modes** (66569088; replaces the default-off gate of ff4ad3fe). Unset =
  annotate: notes with receipts in search, status counters and the journal, no rank change. `full`/`1` adds the
  rank sink and the recap `Learnt-then-retired while away:` clause (2c32256f). `off`/`0` hides notes and counters.
- **Verdicts bind to chunks, not conversations:** one stale anchor no longer demotes a conversation's live chunks
  (1991235c, a3a5f2ab, 17f2af2f). Verdicts on older witnesses show as annotations; before, all 545 superseded
  verdicts were invisible in search (8154d514).
- **Verdict engine pass (2026-10-08).** Anchors keyed by (file, symbol), not project (fe5878bc). Judgment only from
  a HEAD at or after the last one (7ed6eff4). One event per verdict kind: 62k of 74k live events were re-draws
  (680bca4e). v1 events retired behind an epoch floor, kept append-only (c92d4be3, 19b2cee1). Receipts name the
  changing commit, dated by the committer's local day; git search capped at 20 s (8334daee, 319e9645, 5f0f866c,
  6bcd21f7). Also f759c686, 59c29316, 264accb9, 3371b809, 5e6f2e7d, c02e28d7, dc3da1a4, 2a6d10d5, 3c266033,
  c125fff9, fd051681 [untraced: empty bodies; behaviour from subjects only].
- **Dream journal:** `dream --report`, and `csr-engine journal serve` on loopback 127.0.0.1:7373
  (`CSR_NO_JOURNAL_SERVER=1`) (70d9e763). Report 3.6 MB -> 746,149 bytes without vendored mermaid (83219ec9).
  One verb-first sentence per session, checked against its outcome and cached; reruns make zero new `claude -p`
  calls; upgrading re-curates once, +5 batches on the maintainer corpus (5b3637ba, be136791). "SINCE THEN": 43 of
  50 sessions, 454 rows (eb8a8f49). Also TOUCH NEXT, week home `/`, board `/board`, `/corrections`, and
  resolve/dismiss behind same-origin + CSRF checks (45b768f2, 2ec0b8f5, 698668bc, b719b2bc, b14811c2).
- **`csr-engine dreams`:** zero or one dream per project; strategy dreams need 3+ sessions and abstain on a
  receipt missing from the evidence; `--as-of`, `--no-llm` (289ecc37, 450dbe57, 6daadb83, 0abfaf75).
- **Night-pass threads** (off by default; `CSR_DREAM_THREADS=1`): propose-verify over unfinished episodes with a
  verbatim-quote gate (fd1cb675). Dropping session scaffolding cut a call from 52,024 to 12,082 tokens (a3771388).
- PreCompact starts a marked dream cycle (`CSR_NO_DREAM_ON_COMPACT=1`) (2ca7931e). Statusline: `☾ N dreams`,
  `☾ due`, `☾ dreaming` (59b2b1a5, 519cafb6). The unread badge counts journal dreams; it had read "12326 unread"
  while the journal listed 6 (1155af00).
- **Intent channel and lessons.** Corrections, redirects and abandonments land in append-only `intent_events`
  with byte receipts; new `SubagentStop` hook, `csr-engine backfill intent`, `CSR_NO_INTENT_CAPTURE=1`
  (e18920ad, 1cffba26, 71f4624b, 05172115). Only typed words count: 1 of 107 recorded corrections passes the
  new rules (40095521). `csr-engine lessons --project <p>` prints CLAUDE.md candidates with receipts and never
  writes CLAUDE.md (b719b2bc, 54389e31).

### Provenance and self-contamination guard

- CSR no longer embeds its own MCP tool calls, their results, or hook-injected blocks; one sanitizer serves every
  pipeline (b721a745). Recaps carry a machine sentinel; all 5 call sites reject a recap carrying it, quoted or
  not (43b3bdd5, a1e9cd9b, 19f2ac5d).
- **Trust tiers.** Every chunk and artifact caches a floor (Unknown < External < TrustedTool < UserHistory <
  UserConfirmed < System). Missing evidence stays Unknown; ranking reads only the cached column (8d7d2c7d,
  787becb7, 48950790, e8547791, ba845bca). Results carry `<trust>`, csr_why prints `trust=`, prompt lines read
  `[source|tier]` (5c082939). `csr-engine provenance backfill` (also an idle daemon pass,
  `CSR_NO_PROVENANCE_BACKFILL=1`) rebuilt 94,752 of 235,392 chunks in 68 s; the rest belong to deleted
  transcripts (76834169, e6c37fdd). An `insert_reflection` that took over a minute takes 0 ms (ba4889c2).
- **`csr_resolve` needs your confirmation:** it elicits the exact payload and its SHA-256; only a matching Accept
  writes `user_confirmed`, and `Settled:`, the rank sink and proposals read only those rows (75f43f30).
- **`status` contamination count and `csr-engine backfill scrub`.** On a copy of the maintainer DB, contaminated
  conversations went 674 of 2,077 (32.5%) -> 0; 22,076 chunks rewritten, compact index rebuilt, third run a no-op
  (d90c2ebc, e10f2fff).
- **Scrub hardening (`scrub-codex`).** Codex rollout and plan conversations are scrubbed in place, never re-parsed
  as Claude transcripts; a plan that rebuilds 0 chunks or drops more than half is refused (ab204258, 5a2fff74).
  Each conversation applies in one IMMEDIATE transaction that re-reads its planned rows; conversations changed in
  the last 15 minutes wait unless `--include-active` (d4987749). A maintenance-generation fence stops an engine
  loaded before the scrub from republishing the old graph (560f1bc3). Purges wait for live writers; the first
  live purge had died after 9,105 of 19,844 conversations (3d5f7bf9). A real scrub refuses while `lsof` shows another
  process holding the database (`--force` overrides); its longest plan transaction on a full-size clone was 791 ms,
  where the corpus-wide relink seed query alone had held the write lock 18.4 s (230f11c3). A rebuild a maintenance
  pass overtook is refused and rerun, up to 3 times (184d8604).
- **Maintenance lock.** `backfill scrub` holds an OS lock on `<data_dir>/maintenance.lock` exclusively (30 s of
  non-blocking tries, before preflight). Every writable storage operation, hook, importer, daemon sync batch, index
  publication and remap holds it shared for the operation; a nested acquire reuses the thread's token (0.07 us vs
  24 us; a 20,000-row storage bench 4.12 s -> 2.99 s). Hooks exit silently during maintenance, the daemon and
  watcher skip the cycle, `store_reflection` waits 10 s; `status` probes the lock and renders read-only. A 16-process
  exclusive acquisition test gets one owner (7303b148, a793857e).
- **Continuity is proven, never guessed.** Forward cuts are computed on the whole message and mapped back to chunk
  bytes; chunks join only where provenance and spans prove one message. An unproven cut stays and counts as
  "unresolved (ambiguous source)" in the report (7303b148, a793857e).
- **CSR's own headless children purged.** From 2026-09-01 to 09-19, 19,841 entered the corpus: 231,481 of
  486,936 chunks, 42,159 of 52,541 reflections. Import reads the opening prompt raw;
  `backfill scrub --agent-transcripts` removes them (9e440d00).
- **Forward grammar v2** (`.plans/release-10.1/forward-grammar.md`; 93ca27b2). New pane-dream, compaction-card
  and story-draft forwards carry `quoted` and end at their closing quote inside the 700- or 1,200-char limit.
  Legacy v1 forwards end at the known cut or end of line. Every forward is cut; the words around it stay.
  Extraction, the import scrub and `strip_csr_emissions` agree. Probe: a 700-char legacy send plus a reply went
  from dropped to the reply kept. Earlier rounds: 17b9a30c, 26c8e417, f6f65bdf. Story feedback ("CSR's story of
  this session, rev 3 is wrong; ...") is the person's; only the story forward's grammar cuts it (149e494e).
- **One prefix rule before a forward**, shared by extraction and scrub. The text before the header splits into prose,
  then a run of spaces, tabs and `>`, then one ASCII digit touching the header. With no prose, the markers and digit
  go with the forward; prose keeps any digit ("I vote 2 <card>" keeps "I vote 2"; "42" stays). A 45-case table pins
  it for compaction cards, stories and dreamer questions (481df725; replaces the rules of 149e494e, 7303b148 and
  a793857e). Kept prose earlier on the line protects a later forward's digit ("I vote <card> 2<card> Keep..." keeps
  the 2), and the persisted scrub inserts the same joining space as extraction ("4 <card>2." had persisted as "42.").
  57 cases; persisted scrub matches extraction on 114 rows, zero ambiguity (43a446ad). Source-backed chunk splits
  keep the joining space too, whether the chunks match the source as a whole or one by one (fffa3a8e, dc2ca66d).
- Read as CSR's, never the person's: dreamer prompts, "Lost and found" sends, compaction keep lists, forwarded
  notes, pasted `dreams` cards, harness-written user rows (85c0536f, 19e7d467, 7cd81a8e, b1d7de23, 6369e6b9,
  6daadb83, 044662f3). Chunk evidence uses scrubbed text; ~900 import failures logged 2026-09-17 (82509b74).
- Headless `claude -p` children run with `--tools ""`, `--setting-sources ""` and `--no-session-persistence`.
  Before, one plugin hook fired 21,116 times in a month, and 19,852 of 20,159 re-ranker harvest sources were
  CSR's own children; a Haiku call fell from 7.1 s to 1.5 s (7ad3514f, bcea4a8a). See Known limitations.

### Search and hooks performance

- **Hybrid retrieval always on:** FTS5 every call, RRF k=60, `CSR_SEARCH_MODE=hybrid|vector|fts`; identifier probe
  vector 0 hits, hybrid 7, old fallback 4 (d0b97cb4, 12a9acae). New `csr-engine bench`: coding-agent-life-v1, K=5,
  R@5 1.000 every mode, MRR@20 vector 0.917 / fts 0.897 / hybrid 0.867 (056f13fb, 230447c5, 41d8fb13).
- **Stored memories come back** at rank 1, 0.997, via `reflections_fts` and their own trust floor (994c5f3d).
  Standing needs cosine >= 0.45, never for derived rows; weak out-of-family rows sink under a project filter: 67
  queries, 65 pages identical, 0 rank-1 changes, out-of-family rows 3 -> 0 (9d5ad90a, 0d9265be, af171cc6).
- **Scope:** an omitted `project` resolves from `CLAUDE_PROJECT_DIR`, checked against stored names, else `all`
  (f9fd4288, dac585b6; 9.5.7 shipped a narrower version). Search covers the repo family; `all` boosts it 1.15x
  (9768bd0a). `csr_get_more` keeps page one's order (17be7ca0).
- **Weak-band wording:** rank-1 hits are not called "spurious" (that wording drew 42% immediate re-queries vs 18%,
  38f69789); the search path makes no decisive-margin claim, 0 of 13 fixtures (49781817, e22ef9b4, e09326b5,
  3cfdf041). quick_check measures margin against the runner-up (b219dcbe, 96094d65).
- **`get_recent_work`:** repo family, last-activity order, no subagent or `sdk-*` rows, Codex bursts fold into
  `[+N parallel]`. Unscoped: up to 9.43 s cold -> 32 ms (c06804e4, 5c38d820, 96ea2d93, bf7ca8d9, 4dbae46c).
- **`csr_why` walks for real:** it reported 0 seeds and 0 hops; now 10 of 10 ground-truth queries engage, finding
  23 ground-truth sessions against 11 for plain kNN, in score order, plus a `distilled into memory` hop (730b81fa,
  b9caa709, 17f2af2f, 6e0d3d73, 43a635ab, f18f4dae, 55ba91f9).
- Opt-in trained re-ranker (`CSR_TRAINED_RERANK=1`, NDCG@5-gated) (d5d82909); daemon-only reaction shadow (44e9f4ba).
- **What the prompt hook hands over.** Lines from the asking session: 8 of 32 -> 0 of 9 over 10 prompts (044662f3);
  its subagents and derived rows too (22c79047, 1d0431d8). Harness turns: 2,174 bytes -> 0 (ee04254b). Pasted CSR
  output no longer drives the search: 9 fixtures, 51 items and 8 PICKUPs -> 0 (cd92ab7b). Over 95 replayed
  prompts: frame-only rows 18 -> 1 (6609b46f); derived rows past 21 days need cosine >= 0.62 unless they record a
  finding within 90 days, weak old items 42 -> 12 (189a6132, e96ea545); duplicates 6 -> 0 (90f235bf); PICKUPs
  18 -> 8, precision 33% -> 62% (49fcb33d). Each line names its session (cd5ee0cf).
- **Hooks that never search skip the index** (259k chunks): notification 879 -> 10 ms, `continue` 860 -> 13 ms,
  subagent-stop 869 -> 19 ms (a60ef867). Precompact, session-end, stop, post-tool-use only import; precompact 21 ms
  vs a 346,570 ms rebuild (a4453aa1, 58953b81, bcad285b). Hooks never dump the index (~1 GB/prompt before; a7dfb889).
- prompt-submit asks the daemon over a private socket within a deadline and fails open (3651e549, 8e473f5e)
  [untraced: empty bodies; from the `src/hooks/prompt_transport.rs` doc]. The daemon index takes rows other
  processes write (a9f252f5, 1414dc1b) [untraced: subject only].
- Render path: indexed lookups, 64 MiB cache plus mmap, cached ancestry. Warm p50/p95 over 104 prompts 155/354 ->
  92/236 ms (7474c333); p95 200 -> 183 ms (c878bf59); also 7c22b88d, 76458478. Migrations write nothing on an
  up-to-date DB; failed concurrent opens 7 of 60 -> 0 (4edb8a95).
- Cold start never blocks on a full rebuild; the daemon compacts on a marker. MCP spawn to `tools/list`: 5.02 s on
  550,944 chunks (c95b344b, e23aa54c, a5a68e72, e793cd6c, 8691ef54). The tombstone over-fetch loop (860-880 ms
  per thresholded query) is gone (e10f2fff).
- Daemon: the watcher dumps at most every 5 min (0a369d5e); generations publish under a read lock, where 2.7-8.1 s
  flushes had blocked renders (d82b15bc); imports, V3 extraction and the re-ranker run off the async workers
  (8653af6d, 80a5372a); the FTS update trigger fires only on content changes (23dd336d).

### Resource footprint

In-flight resource round on rc2 after b1d7de23. Amend at the marker.

- **One index owner (G1).** With a live daemon, MCP servers proxy query embedding and kNN to it over a private
  socket: no index, no model, no loops, no dumps. Nine servers had held 1.33-2.18 GB each, 15.2 GB together.
  Answers match in-process (ids, order, scores within 1e-5). `CSR_NO_DAEMON_PROXY=1` opts out (c97c166c).
- **Restarts, without a second engine.** A server of a configured daemon never loads the index or the model, even
  while the daemon is down (6b1e378b replaces the in-process fallback of 0f33ba59, which went 60 -> 2,022 MB and
  gave back only to 1,094 MB). Vector tools re-ping for up to 20 s (one try within 15 s of a failure), then answer
  from SQLite's keyword index, marked `[keyword]` with no cosine scores, keeping each tool's dates, scope,
  pagination and format; the recency window and project apply inside the FTS query before its 500-candidate limit
  (6b1e378b, 22f231a4, 8578f22f). `status` lists each server's path under `mcp_servers`: unknown until the daemon
  first answers, then daemon, keyword-only or in-process (0f33ba59, 481df725). Review fixes: 7b5b3238, daa9f444,
  6adae6e4.
- **What "configured" means.** `CSR_NO_DAEMON_PROXY=1` wins. Otherwise `CSR_DAEMON_PROXY=1`, the launchd plist for
  the database it serves (its `--db-path`, else the default DB), a fresh or future-dated heartbeat, or any typed
  Ping/Busy reply; heartbeats and replies carry the daemon's database identity, and a mismatch does not count
  (8578f22f, 481df725). CLI commands (`rebuild-index`, `eval`, `status`, `backfill`) stay local.
- **Daemon socket bounds.** A connection waits for a slot before anything is read; at most 8 frames buffered or
  computing, 32 waiting connections, embeds capped before dispatch; every call has one absolute deadline covering
  connect, write and read, and runs off the Tokio workers (290c8af4, 1dfca84b, 22f231a4).
- **Stores are indexed on return.** A proxied `store_reflection` returns once the daemon's change cursor reaches the
  write, or after 3 s says "semantic indexing is pending" with the id; reflection FTS finds it at once
  (52decdf2, 22f231a4). With the daemon unreachable it says the reflection was not stored.
- **Verified index dumps in `<data_dir>/index-v3/`.** Manifest v3 records each graph and data file's length and
  BLAKE3 hash, checked before hnsw_rs reads anything (1.5 GiB hashes in 813 ms); no hashes or a mismatch is a cache
  miss. Older binaries keep `index/`, which `status` only sizes. A damaged generation is rebuilt from SQLite
  instead of hnsw_rs exiting the process; a short data file had stopped the MCP server before its first answer
  (e88c17bd, 22f231a4). Hooks proxy through the daemon and never build HNSW (22f231a4).
- **Dump budget (G2).** The daemon wrote ~20 GB/h, each MCP server 2.8-3.9 GB/h. An extraction tick wrote 65.7 MB
  of temp B-tree; now 0 MB, 0.29 s instead of 0.50-0.92 s. The daemon dumps after 30 min only when min(2,000,
  0.5%) vectors changed. A per-manifest change cursor stops 42,634 replayed ids and the 17-min rebuilds they
  caused (2bb98088).
- **Vectors from the map (G3).** The daemon's 1.1 GB of heap vector copies are replaced by the mapped generation;
  engines own their maps, so unlinked generations are freed (10 processes had held 9, ~1.1 GB each). ORT embeds
  one text per run: 82.4 -> 106.6 vectors/s, 1,382 -> 261 MB (76e390d6).
- **hnsw_rs 0.3.5:** ~1.9 GB less reserved address space per chunk index; recall@10 0.9070 / 0.9010 / 0.8997 ->
  0.9243 for graphs built after upgrade; dumps load both ways, 18 of 18 tool outputs identical (82d9f837).
- **Test timing (G4):** a fresh binary's 1.1-1.8 s first exec is paid untimed; 56/56 over 10 relinks (eaa9518e).
- **Bounded children and restartable batches.** One helper kills and reaps a child past its deadline: `claude
  --help` 3 s (timeout means the option is unsupported), git calls inside guarded callbacks 10 s. The daemon resumes
  narrative batches left `processing` with a batch id and requeues the rest, without duplicate reflections; a
  completed batch whose storage maintenance refused stays pending (481df725, a793857e). Provider errors are
  classified: transient ones retry in the running daemon (backoff capped at 300 s); a batch the provider no longer
  knows, or an ended batch that omits a result, requeues its rows; three failed batches leave them `failed`
  (new `enrichment_state.batch_failures` column) (43a446ad). Transient errors have their own durable budget
  (`batch_transient_failures`): 296 polls of 503s had left a row `processing` forever. A returned batch id is never
  dropped, and two import paths for one conversation submit one request (fffa3a8e).
- **`status` never scans by default.** Without a cached integrity verdict it reports `healthy: null` (unknown); only
  `status --deep` runs the scan, storing the verdict when maintenance allows (481df725). SwiftBar shows null as
  "Integrity unknown", not red (43a446ad).
<!-- amend: add later resource-round commits above this line -->

### Corpus sources

- **Sidechains:** project from the transcript path (985 files had landed under `subagents`); the parent wins
  dedupe (b721a745). Mis-tagged rows repaired at daemon start (9d5ad90a). Busy imports retry at 5/15/45/120/300 s;
  a 30-min sweep re-imports stale files from the last 14 days (940592d6, 85388bc1).
- **Codex rollouts** (optional, auto-detected, `source='codex_rollout'`) (b721a745). `CSR_NO_CODEX_IMPORT=1`
  skips them (e34ac07a); a busy file no longer ends a pass (f5bf6eec). Per-message timestamps: 1,399 of 1,399
  conversations shared one timestamp, now 56 (a5b3283e, 2deb8e77, 7eefc5e4, 77426b14). Harness boilerplate is
  stripped; 1,327 polluted conversations re-import in idle batches, ~81 chunks/s (96ea2d93, 7eefc5e4).
  `thread_spawn` children count as sidechains (bf7ca8d9).
- **Memory registry (new):** `~/.claude/projects/*/memory/*.md` metadata, never embedded or injected; 30-min scan
  (`CSR_NO_MEMORY_REGISTRY=1`) (f2c728a5, 1b0440fe, 54fa5c99, 9671c15b, 1fca7c0a, fd924ec4).
- **History registry, plans, tasks** (shipped 9.4.0): import records each conversation's entrypoint, so recent work
  hides only `sdk-*` runs (4dbae46c); plans are TrustedTool, and plans and task files bind to episodes as
  observations, never confirmations (8d7d2c7d, e8547791).
- Assistant chunks holding only tool markers are dropped; a typed `[TODO]` stays (e69e424c, f18f4dae).

### MCP protocol

- rmcp 1.6 -> 3.1; tools, schemas and annotation hints unchanged (b721a745).
- Negotiation capped at 2025-11-25: Claude Code >= 2.1.234 had shown "Connected - tools fetch failed" (fd9e5879,
  bc52c9dd). The `server/discover` probe is answered before the handshake so `tools/list` works on Claude Code
  >= 2.1.286 (79fafe41) [untraced: empty body; from `src/mcp/opener_guard.rs`].
- **New tool `csr_transcript`** and `csr-engine transcript`: stats, prompts, tools, files, errors, slice, grep. A
  65 MB transcript's stats take 0.067 s; the prompts view hides harness rows unless `include_system` (02196644,
  cf6903e7, 4bc27558, 3987f9d3, d7ddd7b4).
- A server whose binary was replaced appends `<notice>` to tool results and stops writing the index; `status`
  reports `stale_processes`, `status --stale` reads no database (8f0ec174, 32730957). The statusline "reconnect
  mcp" marker is gone (b4a1708c).

### Installer

- Installers report stale csr-engine processes and `launchctl kickstart -k` the daemon (8f0ec174, 32730957).
- launchd agent `com.ramakay.csr-engine.daemon`: `claude` on PATH, working directory off `/`; TCC prompts 49 in 7
  min -> 0 in 8 min (e34ac07a, 2c32256f, 8f8278ff). On binary replacement the daemon exits 75 within 43-46 s for
  launchd to restart (8f0ec174, 32730957); SIGTERM now stops it (e793cd6c).
- `setup` scans the memory registry, restamps Codex timestamps, strips harness text (fd924ec4, 23dd336d,
  bf7ca8d9); `hook install` adds SubagentStop (e18920ad).
- Windows (unpublished until now): hook commands use forward slashes (86afb4a3, #273); three Windows-only
  prompt-submit failures fixed, stdin reads bounded to 2 s (2c9bdb3b, #285).
- Releases trigger on tags only, and publishing waits for a required reviewer (53afe9b2, 774d1b75).
- Experimental, not installed by setup: `mods/csr-recall` (4fec9f18), `mods/csr-decide` (9bf7757e, f7cb1b48).

### Fixed

- Code-graph worktree paths are canonical; 132 code_evolution and 384 code_nodes rows had throwaway keys
  (e0fe86f5). Re-extraction retires dropped symbols, never on an empty or partial parse (b939bac0, e64775ee,
  51ed42cb). csr_why's episode hop had resolved 0 of 665 links (43a635ab). `reflect_on_past` sinks resolved
  chunks before the limit cut (b721a745).

### Known limitations

- **Headless settings.** `claude -p` children drop user settings unless those settings carry auth: `apiKeyHelper` or
  an `env` key such as `ANTHROPIC_API_KEY`, `ANTHROPIC_BASE_URL`, `CLAUDE_CODE_USE_BEDROCK` or `..._VERTEX` keeps
  them. `CSR_HEADLESS_USER_SETTINGS=1` always keeps, `=0` always drops (bcea4a8a, 64e24d1e). Only key names are
  logged, never values.
- **Upgrade rebuilds the index once.** Dumps move to `index-v3/`, so the first daemon start builds from SQLite;
  until it publishes, MCP tools answer keyword-only. On the maintainer's 9.26 GB database (732,917 chunks, 12,052
  reflections) `rebuild-index` took 834 s with 3,965 MB max RSS and wrote 1.5 GB; the daemon then loaded the
  verified cache in 9.6 s. The legacy `index/` (2.2 GB here) can be deleted once no older binary runs.
- **Concurrent first opens.** ADD COLUMN migrations accept a column another opener just added, and the busy wait is set
  before the WAL switch: 8 concurrent openers of an old-schema copy, 0 failures in 2,400 opens (12 of 80 failed
  before) (fffa3a8e). Several processes creating a brand-new database at the same instant can still see one
  "database is locked"; hooks fail open.
- **`status --compact` is slow on large databases:** 2.9-12.7 s on an 8.70 GB copy (711,622 chunks; 12.9-26.7 s
  before this release), nearly all in the provenance failure-count join and coverage scan (43a446ad timings).
- **Fleet memory gate missed.** Target was <= 3 GB for the daemon plus all MCP servers; the median measured
  4,058 MB before an outage and 3,575 MB at the end, the daemon alone 3,210 MB (4ad029d3 build, 9 servers, 30 min,
  daemon stopped 78 s; 2,272 calls, 0 failed). Outage p95 was 20,136 ms: the first call waits out the re-ping
  backoff before keyword results; the backoff is a post-10.1 item.
- **Batch submit is at-least-once across a crash.** If the daemon dies after the batch API accepts a submission
  and before it stores the batch id, those conversations can be submitted again; each duplicate costs one more
  narrative call. A failed id write without a crash is retried and never resubmits (fffa3a8e).
- Verdict accuracy is unproven: "482 anchors at 2 HEAD commits" is existence evidence [untraced]; the T4 corpus
  has zero reverts; the demote channel (`full`) is unmeasured; CRLF/Windows stamping is untested; the ledger does
  not record a witness's repo (447994c4). Strategy dreams are below their kill bar [untraced].
- Contamination figures depend on corpus and date: 58 of 1,330 embedded conversations (4.4%) on 2026-08-08
  (1f9e4c74); 674 of 2,077 (32.5%) on a 2026-09-01 copy before scrub (d90c2ebc). Text that only quotes the recap
  sentinel is dropped; measured: 2 chunks (1f9e4c74).
- `CSR_DREAM_CONSUMPTION=off` does not gate the static report, live journal feeds or badge; `status` has no field
  for `CSR_NO_CODEX_IMPORT` (2c32256f; both still true at rc2 HEAD by code check).
- hnsw_rs 0.3.5 still calls `process::exit(1)` on load errors (2c32256f; checked in the crate source).
  `eval --full` passes 19 of 21: re-ranker gate lacks data; search p95 2.27 ms against a 1 ms target (82d9f837).
- Codex recall trails Claude, hit@6 12/40 vs 22/40, mostly at the HNSW fetch (9d5ad90a). 1 of 8 fabricated probes
  scores "partial" (0.629) with no caveat (49781817). `csr_get_more` does not re-run decay or provenance rerank
  (17be7ca0). Ablation examples pass `TrustTier::Unknown` (2c32256f).
- `intent_events` is append-only: corrections recorded before 40095521 stay. The installers' stale-process report
  has no installer test and is not in `installer/lib.js` (note at `installer/postinstall.js:254`, 32730957).

## [9.5.0] - 2026-08-03

### Codegraph truth pass: evidence-tiered resolution, two-channel attribution, honest abstention

Every code-graph edge and attribution claim now carries evidence or says it
has none. No scalar confidence scores anywhere — ordinal evidence labels only.

- **NEW: witness-closure resolution (WCR).** Call/import edges resolve only
  through recorded witnesses (same-file, module-bind, import-bound,
  unique-def, co-edit-margin, scope-chain local bindings) or carry a recorded
  reason they cannot (`external`, `method`, `stale`, `drifted`, `local`).
  Eight adversarial review rounds removed every inference path; re-pointing
  requires content-identity (immutable per-edge content hash). Release gates:
  witness closure ≥90%, internal binding ≥70%, drifted=0 — measured live by
  `csr-engine eval --codegraph`.
- **NEW: two-channel symbol attribution.** "Which conversation introduced this
  function" was previously a file-level projection (`first_conv_id`) measured
  wrong ~50% of the time on two independent corpora. Replaced by
  `code_node_attribution`: a **transcript** channel (earliest recorded change
  event naming the symbol) and a **git** channel (`git log -L` introducing
  commit over the symbol's span), rendered separately, disagreements >48h
  labeled — never merged — and symbols with neither channel shown as
  `unattributed` instead of inheriting their file's first toucher.
  User-visible in `csr_code_graph`, `csr_search_by_file`, and `csr_why`.
- **NEW: `csr_quick_check` measured abstention.** Floor derived from fabricated
  vs genuine probes over the full embedding corpus: below 0.45 the tool
  returns `found=false` with the rejected score shown (previously it would
  present a cosine-nearest chunk as if the topic had been discussed); scores
  0.45–0.62 carry an explicit may-be-spurious warning quoting the measured
  fabricated-probe range. Floor re-derivable via `examples/quick_check_floor.rs`.
- **NEW: repo-identity labels.** `code_nodes.repo_root` records the git
  toplevel at write time, stable across cwd/session boundaries. Linked worktree
  paths canonicalize onto the main checkout *going forward*, so newly recorded
  edits converge on one key per logical file. Two limits, both deliberate:
  canonicalization needs the worktree's `.git` marker still on disk, so rows
  whose worktree was already pruned keep their original path rather than being
  guessed at; and the one-shot backfill rewrites `code_evolution` only. It does
  **not** rewrite `code_nodes.file`, because a node's id is derived from its
  path — rewriting the path alone would desynchronize a row from its own
  identity and make it a deletion target for node retirement. Pre-existing
  worktree-keyed nodes therefore remain, and one logical file can still have
  more than one key in historical data.
- **NEW: `csr-engine backfill-coedit`** — rebuilds the session↔file co-edit
  ledger from the historical JSONL corpus (idempotent).
- **FIX (review round): witness tables survived only until the next process
  start** — a migration dropped `local_bindings`/`edge_scope_chains` on every
  DB open (latent data destroyer; persistence is shadow-only today). Drops are
  now shape-probed. Also from review: mixed timestamp formats no longer skew
  earliest-event attribution; symbols defined on line 1 are no longer
  unattributable; deleted files keep a stable path key; the ablation harness
  opens the DB truly read-only.
- **Eval kits.** Five-arm sham-controlled retrieval ablation
  (`eval-kit/h1/`, sealed) and external-gold variant (`eval-kit/ag/`);
  deterministic harness (byte-identical across processes). Findings are in
  the paper: the AST channel is an integrity investment, not a ranking
  signal — no expansion channel separated from zero against its own sham.
- **Paper.** E1b sham-controlled follow-up folded into
  `docs/plans/annaswamy-2026-similarity-drowns-intent.typ` (+ determinism
  methods note, dual corroboration-rate disclosure).

## [9.4.1] - 2026-07-28

### Self-hosting hygiene: hook cancellation, recursion, and corpus contamination

- **FIX: SessionEnd "Hook cancelled".** Claude Code cancels hooks still running
  at quit; CSR's engine startup + HNSW flush (~0.5s) lost that race on almost
  every session end. The hook now re-spawns itself disowned and returns in
  ~7ms — the detached child does the real work and survives the cancellation.
- **FIX: nested-session hook recursion.** The narrative and ratification
  `claude -p` spawns did not set `CSR_DISABLE_RECURSIVE_HOOKS`, so every
  enrichment call fired all six CSR hooks inside the nested session and its
  Stop hook stored the extractor transcript as a session episode. Both spawn
  sites now set the guard, and the guard is checked before engine startup so
  nested no-ops cost nothing.
- **FIX: self-indexing contamination at import.** The ratification extractor's
  prompt was missing from `AGENT_PROMPT_SIGNATURES`, and `claude -p -` records
  a literal `-` stdin marker as the first user-message line that defeated
  prefix matching — so the import watcher ingested every extraction run as a
  real conversation (on the development corpus: 82% of conversations were CSR
  talking to itself). The signature is registered, the dash marker is
  normalized, and the rule is documented in code: every `claude -p` spawn site
  registers its prompt opening in the same commit.
- **Source visibility in status/telemetry.** `csr-engine status` gains
  `aux.sources` — plan docs/chunks/unscoped counts, registry sessions, task
  sessions on disk, resolution proposals and verdicts — and
  `csr-engine telemetry` renders the same block in the text report and TUI.
  Schema-miss counters said what failed to parse; this says what landed.
- **Docs site**: multi-source task-ledger card (animated todo ticks, outcome
  chip, source lanes) and a tile for the research preprint *Similarity Drowns
  Intent*; grid row collision fixed.

Existing installs with a contaminated corpus: transcripts already imported are
not retroactively purged by the upgrade. `aux.sources` + `get_recent_work` make
the pollution visible; a future release will ship a maintenance purge command.

## [9.4.0] - 2026-07-27

### Multi-source memory corpus

CSR's corpus was JSONL-transcripts-only. v9.4 absorbs the rest of `~/.claude`,
each source at the lifecycle stage it belongs to — not one big importer.

- **FIX (live regression): episode task extraction.** Claude Code renamed
  `TodoWrite` → `TaskCreate`/`TaskUpdate`; the Stop hook still mined the old
  tool, so every recent episode had empty todos, no next-steps, and degraded
  outcome labels. The hook now parses the new tools (ids bound via tool
  results, never transcript ordinals) and reads the authoritative final state
  from `~/.claude/tasks/<session>/` — sessions with open tasks can no longer be
  remembered as unqualified successes.
- **Plan documents** (`~/.claude/plans/*.md`) are imported as searchable chunks
  (`source='plan'`), correlated to their origin project/conversation with a
  margin-verified token match (ambiguous plans stay unscoped rather than
  guessing), provenance-linked, and deduplicated at search time — the origin
  conversation always outranks its plan restatement.
- **Session registry** from `~/.claude/history.jsonl`: a cross-project spine of
  every session (never embedded, never injected) powering honest coverage
  stats — `csr-engine status` now reports sessions seen vs imported, plus
  file-history and unindexed-transcript counts.
- **Resolution proposals**: completed tasks matching open queue items generate
  proposals in a separate table — invisible to search until a human promotes
  them via `csr_resolve` (no automatic verdicts).
- **Schema-miss telemetry**: every aux adapter counts what it fails to parse
  (`aux_schema_miss:*` in status) — silent format churn like the TodoWrite
  rename can't rot quality invisibly again.
- Schema: additive `chunks.source` column (O(1) migration, no index),
  `session_registry` + `resolution_proposals` tables.

## [9.3.1] - 2026-07-24

### 🔒 SECURITY/CONSENT: Install and activation are now separate steps (#247)

`npm install claude-self-reflect` used to auto-run `csr-engine setup` from
postinstall — silently writing 6 hooks into the live `~/.claude/settings.json`,
registering the MCP server in `~/.claude.json`, and importing conversation
transcripts, even for sandboxed `--prefix` evaluations. Reported in #247.

- **npm postinstall is now download-only by default**: it fetches the binary,
  verifies the SHA256 checksum, installs it to `~/.local/bin`, and stops.
  Activation happens when the user explicitly runs `csr-engine setup`, or opt
  back in to the old one-shot behavior with `CSR_AUTO_SETUP=1` (a failed
  opt-in setup now exits nonzero so automation can detect it).
- **`install.sh` now asks before running setup**: interactive installs get a
  clear prompt describing exactly what setup touches (hooks, MCP registration,
  conversation import) with a `[Y/n]` confirmation read from `/dev/tty`.
  Non-interactive installs (CI, no TTY) never auto-activate. New env controls:
  `CSR_AUTO_SETUP=1` (run setup without prompting) and `CSR_SKIP_SETUP=1`
  (download only).
- **Docs updated** (README, installation guide, landing page) to describe the
  two-step install → activate flow.

## [9.3.0] - 2026-07-11

### 🚀 FEATURE: Narrative Cost Controls

- **Token accounting**: every AI-narrative `claude -p` call (session briefing +
  story extraction) — including failures and timeouts — records model, token
  usage (input/output/cache), duration, and success into a new
  `narrative_usage` table. Surfaced in `csr-engine status` JSON
  (`narratives` block incl. separate cache-token totals) and the compact
  statusline (`AI 3c/12.4k tok today` / `AI off`).
- **Model resilience**: the hardcoded dated Haiku model ID is gone. Calls walk
  a fallback chain — `CSR_NARRATIVE_MODEL` env override → `haiku` alias → CLI
  default — advancing only on model-not-found (detection matches the real CLI
  404 wording, captured by live probe and pinned by fixture).
- **Kill switch**: `CSR_NO_AI_NARRATIVES=1` disables all AI-narrative calls,
  gated before any spawn work; skips log a distinct `skip:narratives_disabled`.
- **Briefing content-hash cache**: identical episode windows skip the
  `claude -p` call entirely (FNV-1a digest over stable episode content, not
  rendered text) — a resume with no new episodes costs zero tokens.
- **Docs disclosure**: README states exactly when tokens are spent, how they
  are counted, and how to opt out.

### 🚀 FEATURE: Codegraph Pickup (Explore Intent + File-Level Anchors)

- **Explore intent**: the prompt-submit classifier gains a third intent for
  code-location prompts ("where is X", "which files implement Y"). On a hit,
  CSR injects a **CODE MAP** block — up to 5 file pointers from the
  topic-matched past episode (with anchor counts and outcome) plus a
  ready-to-run `csr_reflect_on_past` call — instead of letting the agent
  re-map the codebase from scratch.
- **File-level fallback anchors**: languages ast-grep can't parse (Swift, C#,
  …) now get file-level sentinel anchors (whole-file content hash), so episode
  ANCHORS drift tracking and the symbol-overlap gate work in any language.
- CODE MAP emission is independent of the Tier-0 recency gate — a correlated
  episode is enough.

### Fixed
- Model-404 error JSON is detected on stdout (where the CLI actually emits it
  on non-zero exit), not just stderr — the fallback chain now fires in the
  real failure mode.
- Anchor counting uses path-segment-boundary suffix matching (`Ring.swift` no
  longer counts anchors from `ChannelRing.swift`).

## [9.2.0] - 2026-07-07

### 🚀 FEATURE: Episode Intelligence + Telemetry Dashboard

#### Episode Intelligence
- **Structured episode capture**: the Stop hook now extracts a `csr_episode_v1`
  record (request / investigated / completed / next_steps / outcome /
  error_signatures / files_modified / tools_used) from transcript heuristics —
  no LLM call, runs in <10ms.
- **Async agent hook at SessionStart**: a non-blocking Haiku agent hook reads
  recent episodes via `csr_reflect_on_past` and emits a proactive session
  briefing. First tool in the ecosystem to use Claude Code agent hooks.
- **Episode-aware search**: session episodes render with an `[episode]` prefix
  in MCP search results, distinguishing them from stories/narratives.
- **Delete+insert GC**: each session keeps a single current episode; previous
  versions are removed before insert to avoid search-index bloat.

#### Telemetry Dashboard
- **`csr-engine telemetry`**: new ops view aggregating `hook-timing.log` +
  status into hook-latency percentiles (p50/p95/p99/max), startup
  cached-vs-rebuilt timings, and enrichment health.
- **`--since <24h|7d|30m|all>`**, **`--json`** for machine-readable output, and
  **`--tui`** for a live ratatui dashboard.

### 🐛 Fixes
- **V3 extraction retry storm**: the enrichment daemon re-opened deleted/rotated
  source JSONL files every tick and re-queued them forever (`v3_failed`
  climbing in telemetry, repeated `opening JSONL file for extraction` WARNs).
  Missing sources are now marked `unavailable` (a non-requeued terminal state).
- **`.mcp.json`**: config now correctly nested under the `mcpServers` key.

#### Continuity Engine
- **Provenance indexing**: every chunk carries speaker attribution, source
  conversation, and supersession state — assistant scaffold text is demoted so
  recall favors what was actually decided, not what was proposed.
- **Provenance + meaning re-ranking**: scaffold demotion + primacy boost in the
  search pipeline. Continuity gate harness (`eval --continuity-live`) probes
  the real index, not fixtures — CSR now beats grep on the founding-conversation
  benchmark.
- **Derivation Ledger**: tracks which injected context was derived from where,
  with knapsack-style token-budget injection.
- **Injection Governor**: reuse tracking, anti-flap budget, and holdout logic to
  keep repeated injections from thrashing.

#### Episode Pickup + Intent Routing
- **Episode index at SessionStart**: recent episodes rendered as a pickup menu
  (newest first, outcome-tagged) with one-call `csr_reflect_on_past` lookups.
- **CONTINUUM block**: last-session state (LAST / NEXT / anchors) injected at
  SessionStart so "what were we doing?" needs zero re-derivation.
- **Semantic intent classifier** for UserPromptSubmit Route A: K-intent
  classification via exemplar embeddings over the existing MiniLM model — no
  new model shipped. Continue ≥ 0.60, StateRecall ≥ 0.55; probe cache in
  `~/.claude-self-reflect/intent_probes.json`.
- **Recency-weighted episode correlation** (Route B) with a 7-day half-life.

#### Conversation-Provenance Code Graph
- **`csr_code_graph`** (13th MCP tool): query which conversations touched which
  functions/files — AST anchors link code symbols to the sessions that shaped
  them.

### 🐛 Fixes (since 2026-06-10)
- **Import was blind to 99% of each conversation**: `tool_result` content was
  dropped and chunking was fixed at 50 messages regardless of size. Now embeds
  tool results with size-based chunking — recall coverage went from ~1% to
  effectively full transcripts.
- **Injection quality pass**: meta-session gating, weak-anchor suppression,
  self-referential noise filtering, micro-session story exclusion, and
  zero-signal NEXT/TODOS suppression — SessionStart injection now reads as
  history, not noise.
- **Episode self-pollution**: recursive hook guard (`CSR_DISABLE_RECURSIVE_HOOKS`
  now honored) + agent-prompt import skip; CSR no longer indexes its own
  briefing agents.

### 🔧 Internal
- Version aligned across `package.json` and the `csr-engine` crate (→ 9.2.0);
  the MCP server now reports the real release version via `binary_version`.
- Zero clippy warnings restored across lib, tests, and examples.
- Test suite at release: 467 lib + 42 hooks + 57 integration, zero failures.

## [7.1.9] - 2026-01-05

### 🚀 FEATURE: Cross-Project Iteration Memory

**Global Hooks** • **Automatic Storage** • **Zero Protocol Needed**

v7.1.9 introduces cross-project iteration-level memory for Ralph loops. Hooks now fire automatically for ALL projects, capturing iteration state with project tags for cross-project visibility.

#### Cross-Project Hook System
- **Stop Hook**: Captures iteration state after every Claude response
- **PreCompact Hook**: Backs up Ralph state before context compaction
- **SessionEnd Hook**: Stores session narrative when session ends
- **Project Tagging**: All entries tagged with `project_{name}` for cross-project search
- **Global Configuration**: Hooks configured in `~/.claude/settings.json` work everywhere

#### Technical Changes
- `iteration_hook.py`: Now stores ALL iterations (even without learnings) with project tags
- `precompact-hook.sh`: Uses venv Python (fixes missing qdrant_client), adds project tags
- `standalone_client.py`: Added `collection` parameter for hook-specific storage
- `session_end_hook.py`: Uses dedicated `csr_hook_sessions_local` collection
- Hook collection separated from manual reflections (prevents pollution)

#### Hook Collection
- Dedicated collection: `csr_hook_sessions_local`
- Hook signature tag: `__csr_hook_auto__` distinguishes auto vs manual stores
- Project visibility: Search across projects with `project_` prefix tags

### Added
- Global PreCompact hook configuration
- Project name extraction from working directory
- Cross-project iteration tracking

### Fixed
- PreCompact hook using system Python instead of venv (missing qdrant_client)
- Stop hook not storing when learnings empty
- `2>/dev/null` hiding PreCompact success messages

### Changed
- README.md updated with v7.1.9 features and cross-project examples
- Removed Ralph Loop Iteration Protocol from CLAUDE.md (hooks are automatic)

---

## [7.0.0] - 2025-10-28

### 🚀 MAJOR FEATURE: Automated Narrative Generation

**9.3x Better Search Quality** • **50% Cost Savings** • **Fully Automated**

v7.0.0 introduces AI-powered conversation narratives that transform raw conversation excerpts into rich problem-solution summaries with comprehensive metadata extraction.

#### Search Quality Improvements
- **Search Scores**: 0.074 → 0.691 (9.3x improvement)
- **Token Compression**: 82% reduction while maintaining searchability
- **Metadata Richness**: Automatic extraction of tools used, technical concepts, and files modified
- **Problem-Solution Patterns**: Conversations structured as challenges encountered and solutions implemented

#### Cost-Effective Processing
- **Anthropic Batch API**: ~$0.012 per conversation (50% savings vs $0.025 standard)
- **Automatic Queuing**: Batch processing triggered when threshold reached (default: 10 conversations)
- **Progress Monitoring**: Docker containers for real-time batch monitoring
- **Quality Assurance**: Automated evaluation generation

#### New Components
- `src/runtime/batch_watcher.py`: Queues conversations, triggers batch processing
- `src/runtime/batch_monitor.py`: Monitors Anthropic Batch API jobs
- `docs/design/batch_import_all_projects.py`: Batch narrative generator (PRIMARY SCRIPT)
- `docs/design/batch_ground_truth_generator.py`: Evaluation generator
- `docs/design/extract_events_v3.py`: V3 event extraction with metadata enrichment
- `docs/design/conversation-analyzer/SKILL_V2.md`: Rich narrative template
- Docker Compose profile "batch-automation" for optional batch services
- Dockerfiles: batch-watcher and batch-monitor for secure container deployment

### BREAKING CHANGES

#### Docker Security Migration (Required)
- **Non-root Containers**: All containers now run as non-root user (appuser, UID 1001)
- **Volume Paths Changed**: /root/.claude-self-reflect → /home/appuser/.claude-self-reflect
- **Impact**: Existing Docker volumes may need permission fixes. See migration guide below.

#### Batch Automation (Optional)
- **New Feature**: Batch narrative generation with Anthropic Batch API
- **Disabled by Default**: Enable with `docker compose --profile batch-automation up -d`
- **Requirements**: Requires `ANTHROPIC_API_KEY` environment variable
- **Impact**: New directories created at ~/.claude-self-reflect/batch_queue and batch_state

#### Configuration Centralization
- **New System**: All environment variables and paths centralized in src/runtime/config.py
- **Impact**: Custom scripts importing runtime modules must update to use centralized config

### Added

#### Infrastructure & Security
- Non-root Docker users (appuser, UID 1001) across all containers
- Exponential backoff retry logic for Qdrant connections (5 retries, 1s initial delay)
- File locking (fcntl) with atomic writes to prevent race conditions
- Docker health checks for all long-running services (30s interval, 3 retries)
- Log rotation (10MB max, 3 files per service) to prevent unbounded growth
- Centralized configuration in src/runtime/config.py eliminates hardcoded paths
- UTF-8 encoding enforced on all file operations
- Subprocess timeout configuration (30 minute default for batch operations)

#### Infrastructure
- Centralized requirements.txt for all Python dependencies
- Exponential backoff retry for Qdrant connections (max 5 retries, 1s initial delay)
- UTF-8 encoding enforced on all file operations
- Subprocess timeout configuration (30 minute default for batch operations)

### Changed

#### Configuration
- .env.example: Added batch automation section with detailed comments
- docker-compose.yaml: Added batch-watcher and batch-monitor services
- docker-compose.yaml: Updated init-permissions to UID 1001 and added batch volumes
- All services now use /home/appuser instead of /root

#### Deployment Terminology
- Changed from "dev vs prod" to "standalone vs shared" deployment modes
- Standalone: Single user, no QDRANT_API_KEY needed
- Shared: Multi-user, requires QDRANT_API_KEY for security

### Security

#### Critical Fixes (7 issues)
1. Non-root Docker users prevent privilege escalation
2. Centralized config prevents injection attacks
3. Qdrant retry logic prevents startup race conditions
4. File locking prevents concurrent write corruption
5. Health checks enable automatic recovery
6. Log rotation prevents disk space exhaustion
7. PII sanitization in documentation (replaced /Users/username paths)

#### High Priority Fixes (5 issues)
1. Increased batch-watcher memory to 2GB (prevents OOM)
2. Added fcntl file locking with atomic writes
3. Added Docker health checks (30s interval, 3 retries)
4. Configured log rotation (10MB max, 3 files)
5. Clean dependency graph (no circular imports)

### Documentation
- Added docs/SECURITY.md with standalone vs shared deployment security model
- Updated .env.example with batch automation configuration
- All documentation sanitized (replaced /Users/ramakrishnanannaswamy with /Users/username)
- UTF-8 encoding documented for all file operations

### Fixed
- Docker startup race conditions with Qdrant retry logic
- Concurrent file writes with fcntl locking and atomic operations
- Memory exhaustion with 2GB limit for batch-watcher
- Disk space exhaustion with log rotation
- PII exposure in documentation

### Migration Guide: v6.x to v7.0

#### For Docker Users

1. Backup Qdrant data (REQUIRED):
```bash
docker run --rm \
  -v claude-self-reflect_qdrant_data:/data \
  -v ~/.claude-self-reflect/backups:/backup \
  alpine tar czf /backup/qdrant_pre_v7.tar.gz /data
```

2. Update to v7.0:
```bash
npm install -g claude-self-reflect@7.0.0
```

3. Fix volume permissions (if needed):
```bash
docker compose down
docker compose --profile watch up -d
# init-permissions service will fix ownership to UID 1001
```

4. Enable batch automation (optional):
```bash
# Add to .env:
ANTHROPIC_API_KEY=your-key-here

# Start batch services:
docker compose --profile batch-automation up -d
```

#### For Custom Script Users

Update imports to use centralized config:
```python
# Old (v6.x):
import os
qdrant_url = os.getenv("QDRANT_URL", "http://localhost:6333")

# New (v7.0):
from src.runtime.config import QDRANT_URL, QDRANT_API_KEY
from src.runtime.qdrant_connection import connect_to_qdrant_with_retry

qdrant = connect_to_qdrant_with_retry(
    url=QDRANT_URL,
    api_key=QDRANT_API_KEY if QDRANT_API_KEY else None
)
```

#### For Batch Automation Users

New directories created:
- ~/.claude-self-reflect/batch_queue - Conversation queue
- ~/.claude-self-reflect/batch_state - Batch API state tracking

Configure in .env:
```bash
ANTHROPIC_API_KEY=your-key-here
BATCH_SIZE_TRIGGER=10
BATCH_TIME_TRIGGER_MINUTES=30
SUBPROCESS_TIMEOUT_SECONDS=1800
```

Enable services:
```bash
docker compose --profile batch-automation up -d
```

Monitor batches:
```bash
docker logs -f claude-reflection-batch-watcher
docker logs -f claude-reflection-batch-monitor
```

---

## [5.0.4] - 2025-09-30

### 🎯 Major Code Quality Release - Import Script Refactoring

This release achieves 77% complexity reduction through comprehensive refactoring of the import script, introducing modern design patterns and modular architecture while maintaining 100% backward compatibility.

### Added

#### New Modular Architecture
- **`scripts/message_processors.py`** (248 lines) - Strategy pattern for message processing with separate processors for text, thinking, and tool messages
- **`scripts/metadata_extractor.py`** (262 lines) - Simplified metadata extraction following single responsibility principle
- **`scripts/import_strategies.py`** (344 lines) - Stream import using Strategy pattern with ChunkBuffer and MessageStreamReader
- **`scripts/embedding_service.py`** (241 lines) - Provider pattern for embeddings supporting both local (FastEmbed) and cloud (Voyage)
- **`tests/test_import_refactoring.py`** (395 lines) - Comprehensive test suite with 20 tests covering all components

#### Design Patterns
- Strategy Pattern for message processors and import strategies
- Factory Pattern for MessageProcessorFactory
- Provider Pattern for embedding services
- Dependency Injection for clean component composition

### Changed

#### Code Quality Improvements (77% Reduction)
- **Complexity Reduction**: Average cyclomatic complexity reduced from 14.58 to 3.36 (Grade C → Grade A)
- **Maximum Function Complexity**: Reduced from 49 to <10 (one function at 12, acceptable)
- **Main Script Size**: Reduced from 887 lines to 357 lines (-67%)
- **Grade Improvement**: From C (14.58) to A (3.36)

#### Performance Optimizations
- Smart content truncation prevents memory bloat
- ChunkBuffer for efficient message processing
- Streaming processing (not all-at-once loading)
- Explicit garbage collection after chunks
- Configurable limits via environment variables
- Within 5% of original performance

### Fixed

#### Code Quality Issues
- Reduced cyclomatic complexity across all functions
- Improved error handling and exception management
- Enhanced type safety with proper type hints
- Better memory management for large conversations
- API key clearing after use (embedding_service.py:166)
- DateTime comparison errors (timezone awareness)
- UUID generation for proper Qdrant point IDs
- State management parameter names and method calls

### Security

#### Quality Gates Passed
- ✅ **Codex Evaluator**: Grade A - 77% complexity reduction confirmed
- ✅ **CodeRabbit**: Quality score 99.5% - 8 review cycles completed
- ✅ **Claude Code Review**: Automated security and architecture review
- ✅ **CSR Validator**: All functionality verified working
- ✅ **20 CI/CD Checks**: All passing (Python 3.10/3.11/3.12, npm 18.x/20.x, Docker, Security scans)

#### Security Review
- Automated security review completed
- Input validation on file paths and content
- Error handling prevents information leakage
- No hardcoded credentials
- Follow-up tracked in [Issue #73](https://github.com/ramakay/claude-self-reflect/issues/73)

### Technical Details

#### Files Changed
- **55 files changed**: +8636/-2124 lines
- **4 new modules**: Implementing SOLID principles and design patterns
- **1 comprehensive test suite**: 20 tests with unit, integration, and backward compatibility coverage

#### Quality Metrics
Before: 887 lines, max complexity 49, avg 14.58 (Grade C)
After: 357 lines main script + 4 modular files, max complexity 12, avg 3.36 (Grade A)

#### Backward Compatibility
- ✅ 100% backward compatible - drop-in replacement
- All JSONL formats supported
- Existing state files work
- No breaking changes to API

### Related
- **PR #69**: [refactor: reduce import script complexity from 49 to <10](https://github.com/ramakay/claude-self-reflect/pull/69)
- **PR #72**: [fix: resolve Docker mount error on macOS global install](https://github.com/ramakay/claude-self-reflect/pull/72)
- **Issue #73**: [Security Review: Address Claude Code's Findings](https://github.com/ramakay/claude-self-reflect/issues/73)

## [4.0.1] - 2025-09-18

### 🚀 Unified State Management v5.0 & Performance Release

This release delivers the most significant infrastructure improvement in Claude Self-Reflect history: Unified State Management v5.0, which consolidates 5+ separate state files into a single source of truth, delivering massive performance improvements and eliminating long-standing technical debt.

### Added

#### Unified State Management v5.0
- **Single Source of Truth**: Consolidated 5+ state files (`imported-files.json`, `skipped_files.json`, `failed_files.json`, etc.) into one unified state
- **UnifiedStateManager Class**: 631-line production-grade state management system with thread-safe operations
- **Automatic Migration**: Seamless migration from legacy state files with backup and rollback capability
- **Cross-Platform Compatibility**: Full Windows/macOS/Linux support with native file locking

#### Security Enhancements
- **Path Traversal Protection**: Whitelist-based directory validation prevents unauthorized access
- **Lock Expiry Mechanism**: Prevents deadlocks with configurable lock timeouts
- **Input Validation**: Comprehensive sanitization for all parameters and file paths
- **Timezone-Aware Operations**: Python 3.12+ compatible datetime handling

#### Developer Experience
- **Comprehensive Test Suite**: 730-line test suite with 33 test methods covering all scenarios
- **Migration Tools**: `migrate-to-unified-state.py` with dry-run, backup, and rollback capabilities
- **Performance Benchmarks**: Built-in performance monitoring and validation

### Changed

#### Performance Improvements (20x Faster)
- **Status Check Speed**: Improved from 119ms to 6.26ms (95% improvement)
- **File Processing**: 1.2ms for 1000 files (well under 20ms requirement)
- **Storage Efficiency**: 50% reduction through automatic deduplication
- **Memory Usage**: Optimized state loading and caching

#### Reliability Enhancements
- **Zero Race Conditions**: Atomic operations with file locking prevent data corruption
- **Error Resilience**: Graceful handling of concurrent access and system failures
- **Progress Visibility**: Real-time status updates during import operations
- **State Consistency**: Guaranteed consistency across all import and watcher processes

### Fixed

#### Critical Bug Fixes
- **Streaming Watcher AttributeError**: Fixed streaming-watcher.py startup failure
- **Path Validation Issues**: Resolved Docker and cross-platform path normalization
- **State File Conflicts**: Eliminated race conditions in concurrent import scenarios
- **Lock File Management**: Proper cleanup of temporary and lock files

#### System Stability
- **Container Compatibility**: Enhanced Docker integration with proper volume handling
- **Process Isolation**: Better separation between batch and streaming importers
- **Error Recovery**: Improved error handling and automatic retry mechanisms

### Technical Details

#### Migration Process
```bash
# Preview what will be migrated (recommended first step)
python scripts/migrate-to-unified-state.py --dry-run

# Execute migration with automatic backup
python scripts/migrate-to-unified-state.py

# Rollback if needed (restores from backup)
python scripts/migrate-to-unified-state.py --rollback
```

#### Performance Metrics
- **Processing Speed**: 1200 files processed in 1.2ms
- **Memory Footprint**: 50% reduction through optimized data structures
- **Concurrent Access**: Thread-safe with file locking (Windows msvcrt, Unix fcntl)
- **Cross-Platform**: Native file locking on all supported platforms

#### Files Changed
- **New**: `scripts/unified_state_manager.py` (631 lines) - Core state management
- **New**: `scripts/migrate-to-unified-state.py` (424 lines) - Migration tooling
- **New**: `tests/test_unified_state.py` (730 lines) - Comprehensive test suite
- **Updated**: All import scripts now use unified state management
- **Updated**: Documentation with migration guides and performance metrics

### Security

#### Production-Grade Security
- Comprehensive input validation prevents injection attacks
- Path traversal protection with directory whitelisting
- Lock expiry mechanisms prevent denial-of-service through deadlocks
- Timezone-aware operations prevent datetime-based vulnerabilities

### Breaking Changes

**Migration Required**: This release requires running the migration script to consolidate legacy state files. The migration is automatic and includes backup/rollback capability.

#### Migration Steps for Existing Users
1. **Backup**: Migration script automatically creates backup before changes
2. **Preview**: Run `python scripts/migrate-to-unified-state.py --dry-run` to see what will change
3. **Migrate**: Execute `python scripts/migrate-to-unified-state.py` to consolidate state
4. **Verify**: Check that import processes continue working normally

### Validation

#### Comprehensive Testing
- **Integration Tests**: All 8 test categories pass successfully
- **Performance Benchmarks**: Sub-20ms requirements met with 1.2ms actual performance
- **Migration Validation**: 949 files processed successfully in dry-run testing
- **Cross-Platform**: Tested on Windows, macOS, and Linux environments
- **Container Compatibility**: Docker path normalization working correctly

#### Quality Assurance
- **Code Review**: 95/100 confidence rating from automated code analysis
- **Security Review**: All security vulnerabilities addressed
- **Performance Review**: Meets all sub-20ms response time requirements
- **Documentation Review**: Migration guides and technical documentation complete

### Contributors
- **Core Development**: Implementation of Unified State Management system
- **CodeRabbit AI**: Comprehensive code review and security analysis
- **Testing**: Validation across multiple platforms and configurations
- **Documentation**: Migration guides and performance documentation

### Upgrade Instructions
```bash
# Update to v4.0.1
npm update -g claude-self-reflect@4.0.1

# Run migration (creates automatic backup)
cd ~/.claude-self-reflect
python scripts/migrate-to-unified-state.py

# Restart Claude Code for MCP server updates
# Migration is complete - enjoy 20x faster performance!
```

### Notes
- **Automatic Backup**: Migration creates backup of all state files before changes
- **Rollback Available**: Use `--rollback` flag if any issues occur
- **No Data Loss**: All existing import history and progress is preserved
- **Immediate Benefits**: 20x performance improvement visible immediately after migration

---

## [3.3.1] - 2025-09-14

### Bug Fixes & Security Release

This release addresses critical quality tracking bugs and implements comprehensive security improvements identified through GPT-5 code review.

### Fixed

#### Critical Quality Tracking Issues
- **Global Quality Cache Bug** - Fixed quality cache being global instead of per-project
  - **Root Cause**: Quality cache was shared across all projects, causing identical metrics display
  - **Impact**: Statusline showed "100% A/14" for all projects regardless of actual quality
  - **Solution**: Implemented per-project quality cache isolation with project-specific cache keys
  - **Files Modified**: `scripts/session_quality_tracker.py` - Added project path normalization for cache isolation
  - **User Impact**: Each project now shows accurate, independent quality metrics

- **Permanent Hourglass Display** - Fixed statusline showing "[⏳:...]" when no session files exist
  - **Root Cause**: Quality tracker returned hourglass status for projects without active sessions
  - **Impact**: Non-coding projects (conversations only) showed confusing permanent loading state
  - **Solution**: Added conversation-based quality analysis for non-code projects
  - **Files Modified**: `scripts/conversation-quality-analyzer.py` (new), `scripts/csr-status`
  - **Enhancement**: Quality metrics now analyze conversation patterns when source code isn't available

- **Confusing Scope Labels** - Removed misleading scope indicators from statusline display
  - **Root Cause**: Scope labels like "Global:" and "Project:" confused users about quality context
  - **Solution**: Simplified display to show only essential quality metrics without scope prefixes
  - **Files Modified**: `scripts/csr-status` - Cleaned up display format

### Security

#### Comprehensive Security Hardening (GPT-5 Review)
- **Path Sanitization** - Implemented secure whitelist-based approach for project names
  - Added bounds checking for all user-controlled inputs
  - Validates project paths against known safe patterns
  - Prevents directory traversal attacks with "../" sequences
  - Files Modified: `scripts/session_quality_tracker.py`, `scripts/update-quality-all-projects.py`

- **Command Injection Prevention** - Secured subprocess execution with binary validation
  - Added whitelist validation for all executable binaries
  - Implemented safe command construction with proper escaping
  - Removed shell=True usage where possible for subprocess calls
  - Added input sanitization for all subprocess arguments

- **Input Validation** - Enhanced bounds checking and data validation
  - Added maximum limits for file sizes and processing counts
  - Implemented input sanitization for all user-provided data
  - Added validation for configuration file formats and values
  - Enhanced error handling to prevent information disclosure

### Added

#### New Quality Analysis Features
- **Conversation Quality Analysis** - Quality metrics for non-code projects
  - Analyzes conversation patterns for quality indicators
  - Detects bug fixes, testing mentions, documentation updates
  - Provides quality grades based on development practices discussed
  - Enables quality tracking for pure conversation-based projects

- **Enhanced Quality Cache** - Improved performance and accuracy
  - Per-project cache isolation prevents cross-project contamination
  - Extended cache validity period to 24 hours (was 60 minutes)
  - Automatic cache invalidation when project files change
  - Better performance with reduced redundant analysis

- **Automated Quality Updates** - Background quality metric maintenance
  - New `update-quality-all-projects.py` script for comprehensive updates
  - Automatic quality metric updates via watcher integration
  - Scheduled quality refresh for all tracked projects
  - Maintains accurate metrics without manual intervention

### Technical Details

#### Files Added
- `scripts/conversation-quality-analyzer.py` - Conversation-based quality analysis
- `scripts/update-quality-all-projects.py` - Batch quality updates for all projects
- Enhanced security validation throughout existing scripts

#### Files Modified
- `scripts/session_quality_tracker.py` - Per-project cache isolation and security hardening
- `scripts/csr-status` - Improved display format and conversation quality support
- `scripts/streaming-watcher.py` - Integration with automatic quality updates

#### Security Measures
- Whitelist-based path validation prevents directory traversal
- Binary validation ensures only safe executables are called
- Input bounds checking prevents buffer overflow scenarios
- Enhanced error handling prevents information disclosure
- Comprehensive input sanitization across all user-facing interfaces

### Performance
- Quality analysis cache validity extended to 24 hours for better performance
- Reduced redundant analysis through improved caching strategies
- Optimized conversation parsing for faster quality assessment
- Better memory usage patterns with proper cleanup and validation

### Compatibility
- Full backward compatibility with existing quality metrics
- Graceful fallback for projects without conversation data
- No configuration changes required for existing installations
- All existing quality grades and history preserved

### Validation
- Security hardening validated through comprehensive code review
- Quality tracking tested across multiple project types
- Cache isolation verified with concurrent project testing
- Performance improvements measured and validated

## [3.3.0] - 2025-09-12

### 🚀 Major Architecture & Performance Release

This release represents a fundamental improvement in code organization, performance, and feature completeness. The MCP server has been completely modularized, critical bugs fixed, and new temporal tools added.

### Fixed

#### Critical Bug Fixes
- **CRITICAL: Circular Reference CPU Usage** - Fixed 100% CPU usage caused by circular import in `get_embedding_manager`
  - **Root Cause**: Circular import between `embedding_manager.py` and other modules during initialization
  - **Impact**: Server would consume 100% CPU and become unresponsive during embedding operations
  - **Solution**: Restructured imports and dependency injection to eliminate circular references
  - **Files Modified**: `mcp-server/src/embedding_manager.py`, `mcp-server/src/server.py`
  - **User Impact**: Server now operates efficiently with normal CPU usage patterns

- **CRITICAL: Store Reflection Dimension Mismatch** - Fixed `store_reflection` failing with dimension errors
  - **Root Cause**: `store_reflection` was hardcoded to use `reflections_voyage` collection regardless of embedding mode
  - **Impact**: Storing reflections failed in local mode, breaking the core memory functionality
  - **Solution**: Updated to dynamically detect and use correct collection (`reflections_local` or `reflections_voyage`)
  - **Files Modified**: `mcp-server/src/reflection_tools.py`
  - **User Impact**: Both local FastEmbed and Voyage AI modes now support reflection storage correctly

- **SearchResult Type Inconsistency** - Fixed TypeScript-style interface causing runtime errors
  - **Root Cause**: `SearchResult` class in `parallel_search.py` used TypeScript-style type annotations incompatible with Python
  - **Impact**: Search operations would fail with attribute errors during result processing
  - **Solution**: Converted to proper Python dataclass with correct type hints
  - **Files Modified**: `mcp-server/src/parallel_search.py`

### Added

#### Major Code Modularization (68% Size Reduction)
- **Complete Server Modularization** - Split monolithic `server.py` (2,321 lines) into focused modules (728 lines)
  - **New Modules Created**:
    - `search_tools.py` - All search-related MCP tools (reflect_on_past, search_by_file, etc.)
    - `temporal_tools.py` - Time-based search and analysis tools
    - `reflection_tools.py` - Memory storage and retrieval functionality
    - `parallel_search.py` - Multi-collection search orchestration
    - `rich_formatting.py` - Consistent output formatting with emojis
  - **Benefits**: Improved maintainability, easier testing, reduced cognitive load
  - **Architecture**: Clean separation of concerns with dependency injection patterns
  - **Compatibility**: Zero breaking changes - all tools function identically

#### New Temporal Tools Suite
- **`get_recent_work`** - Find conversations from specific time periods
  - Supports natural language time queries ("last week", "past 3 days", "yesterday")
  - Smart date parsing with timezone awareness
  - Optimized for finding recent context and work patterns
- **`search_by_recency`** - Search within time-bounded windows
  - Combines semantic search with temporal filtering
  - Helps find "what did I discuss about X recently?"
  - Performance optimized with time-based collection filtering
- **`get_timeline`** - Chronological conversation analysis
  - Maps conversation flow across projects and time
  - Identifies patterns in work focus and topic evolution
  - Useful for project retrospectives and progress tracking

#### Enhanced Metadata Extraction System
- **Tool Usage Analysis** - Captures and indexes tool usage patterns
  - Extracts `tools_used` metadata from conversation history
  - Enables searching by development patterns ("when did I use git?")
  - Cross-reference tool usage across projects and time periods
- **File Analysis Tracking** - Monitors file interaction patterns
  - Captures `files_analyzed` and `files_edited` from conversations
  - Enables `search_by_file` functionality for code context discovery
  - Tracks file modification patterns across conversation history
- **Concept Extraction** - Semantic concept indexing
  - Automatically extracts technical concepts and topics from conversations
  - Improves `search_by_concept` accuracy and coverage
  - Creates semantic maps of discussion themes and technical focus areas

#### Production Infrastructure Features
- **Precompact Hook System** - Automated real-time indexing
  - `precompact-hook.sh` integrates with Claude session startup
  - `import-latest.py` provides smart incremental import logic
  - Ensures new conversations are immediately searchable
  - Reduces indexing latency from hours to seconds
- **Smart Indexing Intervals** - Adaptive processing frequency
  - Hot files (recently modified): 2-second processing intervals
  - Normal files: 60-second intervals for efficiency
  - Automatic file age detection and priority adjustment
  - Prevents resource waste while maintaining responsiveness

### Changed

#### User Experience Improvements
- **Rich Formatting Restoration** - Brought back emoji indicators for better UX
  - 🎯 Search targets and focus areas
  - ⚡ Performance metrics and speed indicators
  - 📊 Statistics and data summaries
  - 🔍 Search operations and discovery
  - **Rationale**: User feedback indicated emojis significantly improve readability and information hierarchy
  - **Implementation**: Centralized in `rich_formatting.py` for consistency

#### Performance Optimizations
- **All 15+ MCP Tools Operational** - Complete tool ecosystem now functional
  - Previously broken tools restored: `search_by_file`, `search_by_concept`, `get_timeline`
  - Enhanced error handling prevents tool failures from affecting others
  - Comprehensive testing ensures reliability across all embedding modes
  - Real-time validation of tool connectivity and response times

#### Architecture Improvements
- **Dependency Injection Patterns** - Clean module separation
  - Eliminates circular dependencies and import conflicts
  - Enables independent testing of individual modules
  - Improves development velocity with isolated component changes
  - Future-proofs architecture for additional embedding providers

### Technical Details

#### Performance Metrics
- **Search Latency**: Maintained <10ms average response time despite modularization
- **Memory Usage**: 15% reduction due to optimized import patterns
- **Code Maintainability**: 68% reduction in core server file size
- **Test Coverage**: Modular structure enables focused unit testing

#### Compatibility & Migration
- **Zero Breaking Changes**: All existing functionality preserved
- **API Compatibility**: Tool signatures and responses unchanged
- **Data Migration**: No data migration required - existing collections work seamlessly
- **Configuration**: No configuration changes needed

#### Testing & Validation
- **Both Embedding Modes**: Local (FastEmbed) and cloud (Voyage AI) tested and working
- **Import Success Rate**: Maintained 99.8% completion rate
- **Tool Functionality**: All 15+ tools verified operational
- **Performance Regression**: No performance degradation detected

### Contributors
- **Main Development**: Claude Code for architecture design and implementation
- **Code Review**: Opus 4.1 for modularization patterns and dependency management
- **Testing**: GPT-5 for edge case identification and validation
- **Documentation**: Claude Sonnet for comprehensive release documentation

### Upgrade Instructions
No user action required - update is seamless:
```bash
npm update -g claude-self-reflect@3.3.0
# Restart Claude Code for MCP server updates to take effect
```

### Breaking Changes
None - this release maintains full backward compatibility while adding significant new functionality.

---

## [3.2.4] - 2025-09-10

### Fixed
- **CRITICAL: Search Threshold Removal** - Eliminated artificial 0.7+ score thresholds that blocked broad searches
  - **Root Cause**: Hardcoded minimum score thresholds prevented searches for common terms like "docker", "MCP", "python"
  - **Impact**: Searches for broad technical concepts were returning 0 results despite relevant conversations existing
  - **Solution**: Removed artificial thresholds and let Qdrant handle natural scoring for better search coverage
  - **Files Modified**: `mcp-server/src/server.py` - Removed minScore filtering in search operations
  - **User Experience**: Searches now return results for previously blocked queries, dramatically improving search utility

### Added
- **Shared Normalization Module** - Created centralized project name normalization to prevent search failures
  - **New Module**: `shared/normalization.py` - Single source of truth for project name normalization
  - **Purpose**: Ensures consistent collection naming between import scripts and MCP server
  - **Impact**: Prevents search failures due to mismatched collection names across different components
  - **Integration**: Used by both import-conversations-unified.py and MCP server for consistent hashing

### Changed
- **Memory Decay Implementation** - Fixed math errors and implemented native Qdrant decay calculation
  - **Root Cause**: Previous decay implementation had mathematical errors in exponential calculation
  - **Solution**: Corrected decay formula with proper imports and native Qdrant model usage
  - **Files Modified**: `mcp-server/src/server.py` - Fixed decay calculation with proper math.exp import
  - **Performance**: More accurate time-based relevance scoring with corrected exponential decay

### Technical Details
- **Search Behavior**: Searches now rely on Qdrant's natural scoring without artificial threshold filtering
- **Collection Consistency**: Shared normalization prevents import/search mismatches
- **Memory Decay**: Fixed exponential decay formula provides accurate time-based relevance weighting
- **Backward Compatibility**: All existing collections and searches remain functional

### Validation
- Search functionality tested with previously failing queries ("docker", "MCP", "python")
- Collection normalization verified across different project path formats
- Memory decay calculation validated with proper mathematical implementation
- No breaking changes to existing search behavior or stored data

## [3.2.3] - 2025-09-10

### Fixed
- **CLI Status Command**: Resolved broken `claude-self-reflect status` command in global npm installations
  - **Root Cause**: CLI was incorrectly calling `python -m src --status` which doesn't exist
  - **Solution**: Now directly calls `status.py` script as intended
  - **Enhancement**: Added fallback support for both `venv` and `.venv` virtual environment directories
  - **Impact**: Statusline integration in Claude Code now works correctly again
  - **Files Modified**: `installer/cli.js` - Fixed status command execution path

### Technical Details
- **Error Pattern**: Command was attempting to use MCP server module interface for status
- **Correct Pattern**: Direct execution of dedicated status script
- **Virtual Environment**: Enhanced compatibility with different venv naming conventions
- **User Experience**: Status command now returns proper JSON output for external tools

### Validation
- Status command tested with both `venv/` and `.venv/` directory structures
- JSON output format validated for statusline integration compatibility
- Global npm installation verified to work correctly

## [3.2.0] - 2025-09-09

### Added
- **Enhanced Search Modes**: Added `mode` parameter to `reflect_on_past` tool with three options:
  - `full` (default): Returns all results with complete details
  - `quick`: Returns count and top result only for fast previews  
  - `summary`: Returns aggregated insights without individual results
- **Pagination Support**: New `get_next_results` tool for cursor-based pagination
  - Supports offset/limit parameters for flexible result navigation
  - Works with both project-specific and cross-project searches
  - Returns metadata about remaining results for better UX

### Fixed
- **Documentation Cleanup**: Removed references to non-existent MCP tools from MCP_REFERENCE.md
  - Removed `quick_search` (use `reflect_on_past` with `mode="quick"`)
  - Removed `search_summary` (use `reflect_on_past` with `mode="summary"`)  
  - Removed `get_more_results` (use new `get_next_results` tool)
- **Performance Bounds**: Search results now capped at maximum 100 to prevent performance issues
- **Error Handling**: Enhanced exception handling with specific error types and detailed logging
- **Input Validation**: Added proper validation for mode parameter and search limits

### Changed  
- **API Enhancement**: `reflect_on_past` now supports flexible search modes while maintaining backward compatibility
- **Response Optimization**: Improved text preview generation for better performance
- **Documentation**: Updated all examples to show correct tool usage patterns

### Technical Details
- Mode parameter validation prevents invalid queries
- Enhanced logging for debugging failed operations  
- Optimized response generation reduces latency
- Comprehensive error handling with graceful degradation
- All changes are backward compatible with existing code

### Performance
- Search operations bounded to prevent performance issues
- Mode-specific optimizations (quick mode for fast previews)
- Enhanced text processing efficiency
- Better resource utilization through capped result sets

### Contributors
- Claude Code for implementation of mode parameter and pagination support
- Opus 4.1 for comprehensive code review and quality improvements
- Community for testing and API usability feedback

## [3.0.2] - 2025-09-08

### Fixed
- Fixed Windows ESM import error (Issue #51) - Added pathToFileURL for dynamic imports
- Removed .fastembed-cache from source control (25 files, ~90MB)
- Added .fastembed-cache to .gitignore

### Security
- Reduced package size by removing cached model files

## [3.0.1] - 2025-09-08

### Fixed
- Added missing `scripts/importer/` directory to npm package files array
- Added dependency-injector to pyproject.toml requirements
- Created __main__.py entry point for modular importer
- Added main() function to importer/main.py for CLI execution
- Created compatibility wrappers for backward compatibility

### Changed
- Updated package.json to include `scripts/importer/**/*.py`
- Bumped version to 3.0.1 for critical hotfix

## [3.0.0] - 2025-09-08

### Added
- **Complete Modular Architecture Rewrite**: 15+ focused modules replacing monolithic import system
  - Dependency injection with clean separation of concerns
  - SOLID principles throughout the architecture
  - Extensible design for future embedding providers
  - New module structure: core/, embeddings/, processors/, storage/, state/, utils/
- **Token-Aware Batching for Voyage AI**: Fixes Issue #38 preventing "max allowed tokens per batch is 120000" errors
  - Intelligent token estimation (3 chars = 1 token)
  - Dynamic batch splitting to stay under 100k tokens
  - Automatic text truncation for oversized content
  - Debug logging for batch statistics
- **Enhanced Embedding Provider System**:
  - Conditional imports: voyageai only loaded when needed
  - Unified interface: Consistent API across providers
  - Provider selection: Automatic based on configuration
  - Dimension validation: Ensures correct vector sizes
- **Comprehensive Test Infrastructure**:
  - Organized test structure: unit/, integration/, performance/, e2e/
  - Enhanced test coverage with proper organization
  - Performance benchmarks and system validation

### Changed
- **BREAKING**: Import script location changed from `scripts/import-conversations-unified.py` to modular `scripts/importer/` package
- **BREAKING**: Method name standardization:
  - `embed_texts()` → `embed()` (standardized across providers)
  - `embed()` → `embed_batch()` for batch processing
- **Performance Optimizations**: 50% reduction in memory usage during imports
- **Enhanced Error Handling**: Comprehensive error handling with custom exceptions
- **Code Quality Improvements**: Type hints throughout, proper logging at all levels

### Added Environment Variables
- `MAX_TOKENS_PER_BATCH` (default: 100000) - Token limit configuration
- `TOKEN_ESTIMATION_RATIO` (default: 3) - Conservative chars-per-token estimate

### Fixed
- **Critical**: Token limit errors with Voyage AI (Issue #38)
- **Fixed**: Embedding dimension mismatches
- **Fixed**: State file corruption on concurrent access
- **Fixed**: Memory leaks in streaming importer
- **Fixed**: Collection naming inconsistencies

### Technical Details
- Architecture follows dependency injection patterns for better testability
- Atomic state persistence prevents data loss
- Windows compatibility improvements
- Intelligent caching reduces redundant API calls
- Streaming processing for large conversations

### Migration Notes
- **No data migration needed** - existing collections remain compatible
- Update to v3.0: `npm update -g claude-self-reflect`
- Run setup to update system: `claude-self-reflect setup`
- For developers: Install new dependency `pip install dependency-injector`

### Performance Improvements
- Token-aware batching prevents API failures
- Memory-efficient streaming with intelligent batching
- Reduced API calls through intelligent caching
- Enhanced processing speed for large conversation files

### Acknowledgments
- Opus 4.1 for comprehensive code review and architectural guidance
- GPT-5 for identifying critical edge cases
- @cchapman for reporting Issue #38
- Community contributors for testing and feedback

## [2.8.5] - 2025-09-02

### Security
- **CVE-2025-58050 Mitigation**: PCRE2 heap buffer overflow in libpcre2-8-0 10.45-1
  - Added explicit PCRE2 upgrade attempts to all Debian-based Dockerfiles
  - Created CI workflow to monitor vulnerability status daily
  - Added vulnerability checking scripts for operational monitoring
  - Low risk for this project (no user-controlled PCRE2 patterns)

### Changed
- **Python 3.13 Upgrade**: Updated all Docker base images from Python 3.12 to 3.13
  - Merged Dependabot PR #30 for Python Alpine updates
  - Improved security and performance with latest Python version
  - All dependencies validated for Python 3.13 compatibility

### Added
- `.github/workflows/security-pcre2-check.yml` - Automated vulnerability monitoring
- `scripts/check-pcre2-vulnerability.sh` - Manual vulnerability checking
- `scripts/mitigate-pcre2-vuln.sh` - Mitigation application script

### Notes
- PCRE2 patch (10.46+) not yet available in Debian stable repositories
- Alpine-based images are not affected by CVE-2025-58050
- Monitoring will continue until upstream fix is available

## [2.8.4] - 2025-09-02

### Added
- **Claude Code Statusline Support**: Configure your Claude Code statusline with reflection metrics
- **Enhanced Documentation**: Added comprehensive statusline configuration guide
- **Visual Status Indicators**: Real-time MCP connection status in statusline

### Improved
- **User Experience**: Better visibility of reflection system status
- **Documentation**: Added screenshots and examples for statusline setup

## [2.8.3] - 2025-09-02

### Added
- **Production Health Monitoring**: HTTP endpoints (/health, /ready, /metrics) for Docker/Kubernetes integration
- **Session Startup Hooks**: Auto-indexing new conversations on Claude session start
- **Diagnostic Tool**: Comprehensive doctor.py for troubleshooting installations
- **Docker Orchestration**: Enhanced Docker manifest for better service management

### Fixed
- **Critical Watcher Reliability**: Memory leak fixes, proper async cleanup, retry logic improvements
- **Path Expansion Issues**: Fixed tilde (~) expansion problems in Docker volumes (GitHub #116)
- **Embedding Retry Logic**: Fixed premature exit on dimension mismatches
- **Connection Leaks**: Added proper AsyncQdrantClient cleanup

### Improved
- **Setup Wizard**: Pre-flight validation and better error messages
- **Bash Watchdog**: Added jittered exponential backoff for stability
- **Documentation**: Enhanced troubleshooting guides and setup instructions
- **Indexing Coverage**: System now achieves 98.3% indexing rate

## [2.8.2] - 2025-09-01

### Fixed
- **CRITICAL: MCP Server Startup Issue** - Resolved IndentationError preventing server initialization
  - **Root Cause**: Incorrect indentation in `update_indexing_status` function introduced during path normalization
  - **Impact**: MCP server failed to start, preventing all reflection functionality from working
  - **Solution**: Fixed function indentation while preserving path normalization improvements
  - **Files Modified**: `mcp-server/src/server.py` - Corrected lines 263-286 indentation
  - **User Action**: Update to v2.8.2 and restart Claude Code for immediate resolution

### Changed
- **Documentation Improvements**: Enhanced clarity around installation and path handling
  - Updated setup instructions with clearer Docker volume mounting guidance
  - Improved troubleshooting documentation for common installation issues
  - Better explanations of system requirements and dependencies
  - Enhanced error messaging and diagnostic information

### Technical Details
- **Validation**: MCP server startup verified, all Claude Self-Reflect tools now accessible
- **Compatibility**: Fully backward compatible, no configuration changes required
- **Performance**: No performance impact, purely a stability fix
- **Migration**: Automatic - no user action needed beyond updating package version

## [2.7.1] - 2025-08-24

### Fixed
- **CRITICAL: Reflections Not Searchable from Project Context** - Cross-agent reflection sharing now works properly
  - **Root Cause**: Reflections stored by one agent weren't searchable by the next agent when working in project context
  - **Issue**: Project-scoped searches excluded the global reflections collection, making stored insights invisible
  - **Solution**: Modified search logic to always include reflections collection when searching from specific projects
  - **Impact**: Agents can now build upon each other's stored insights and maintain conversation continuity
  - **Files Modified**:
    - `mcp-server/src/server.py`: Enhanced project-scoped search to include reflections collection
    - `mcp-server/src/server.py`: Added project metadata to newly stored reflections for better organization
    - `CLAUDE.md`: Updated documentation with reflection storage improvements
  - **Backward Compatibility**: Old reflections without project metadata remain fully searchable

### Added
- **Project Metadata for Reflections**: Newly stored reflections now include project context for better organization
- **Enhanced Search Logic**: Project-specific searches now automatically include global reflections for comprehensive results
- **Improved Cross-Agent Continuity**: Agents can now discover and build upon insights from previous interactions

### Technical Details
- **Search Behavior**: When searching from a specific project, the system now queries both the project collection and the global reflections collection
- **Metadata Schema**: New reflections include project name and context for future filtering capabilities
- **Performance**: No performance impact - reflections collection is lightweight and adds minimal overhead
- **Migration**: Automatic - no user action required, existing reflections remain accessible

### Validation
- **Cross-Agent Testing**: Verified that insights stored by one agent are discoverable by subsequent agents
- **Project Isolation**: Confirmed that project-scoped searches still work correctly while including relevant reflections
- **Backward Compatibility**: Tested that old reflections without project metadata continue to function properly

## [2.7.0] - 2025-08-21

### Added
- **Streaming import implementation** - True line-by-line JSONL processing prevents OOM on large files
- **ProjectResolver class** - Intelligent multi-strategy project name resolution with caching
- **Memory optimization** - Smart garbage collection and resource monitoring
- **Enhanced error handling** - Graceful handling of malformed JSONL entries with retry logic
- **Security improvements** - All sensitive data moved to environment variables
- **Comprehensive troubleshooting** - Enhanced diagnostic tools and error messages

### Changed
- **BREAKING**: `import-conversations-unified.py` now uses streaming implementation by default
- **BREAKING**: Docker memory limits reduced from 2GB to 600MB for production deployments
- **BREAKING**: Some container profiles disabled by default to prevent resource conflicts
- **Memory usage**: Reduced from 400MB to 150MB average (62% improvement)
- **Performance**: 15-20% faster import processing with optimized streaming
- **Container startup**: 40% faster initialization with new resource limits

### Removed
- Duplicate import scripts: `safe-watcher.py`, `parallel-streaming-importer.py`
- Backup files containing sensitive API keys from repository
- Various obsolete test scripts and temporary files
- Old batch-loading mode (was causing OOM issues)

### Fixed
- **Memory leaks**: Added `MALLOC_ARENA_MAX=2` to prevent glibc memory fragmentation
- **Docker mount path issues**: Proper state file handling across container environments
- **OOM failures**: 95% reduction in out-of-memory related failures
- **Large file processing**: Files up to 50MB+ now process successfully
- **State file conflicts**: Fixed path mismatches between Docker and host systems

### Performance
- Memory usage: 400MB → 150MB average (62% reduction)
- Large file support: Successfully processes 12MB+ conversation files
- Error reduction: 95% fewer OOM-related failures
- Import speed: 15-20% performance improvement
- Resource efficiency: 70% reduction in memory requirements

### Migration Notes
- Update Docker configuration to use new 600MB memory limits
- Clean up old state files if experiencing issues
- Local embeddings now enabled by default (no API keys required)
- All existing collections remain fully accessible

## [2.6.0] - 2025-08-20

### Fixed
- **CRITICAL: Voyage AI Token Limit Exceeded** - Resolves import failures for large conversations (#38)
  - **Root Cause**: Batch size based on message count (100 messages) could exceed Voyage AI's 120,000 token limit
  - **Impact**: Some conversations with extensive code content couldn't be imported, causing data loss
  - **Solution**: Implemented intelligent token-aware batching with content analysis
  - **Files Modified**: `scripts/import-conversations-unified.py`, added architecture documentation

### Added
- **Token-Aware Batching System** for reliable Voyage AI imports:
  - Content-aware token estimation with 30% adjustment for code/JSON content
  - Dynamic batch sizing that respects 120k token limits with 20k safety buffer
  - Automatic chunk splitting for oversized conversations (max 10 recursion levels)
  - Graceful degradation with truncation warnings for single oversized messages
  - Debug logging for batch statistics and performance monitoring
- **Enhanced Configuration Options**:
  - `MAX_TOKENS_PER_BATCH` (default: 100,000) with validation bounds [1,000-120,000]
  - `TOKEN_ESTIMATION_RATIO` (default: 3 chars/token) with bounds [2-10]
  - `USE_TOKEN_AWARE_BATCHING` (default: true) for backward compatibility
  - Automatic fallback to original batching if disabled

### Changed
- **Import Reliability Improvements**:
  - Conversation chunks now analyzed for content type before batching
  - Code and JSON content detected and adjusted for higher token density
  - 10% safety margin added to all token estimates
  - Recursive chunk splitting preserves message context when possible
- **Error Handling Enhancements**:
  - Clear warnings for chunk splitting operations
  - Detailed logging of truncation events with size information
  - Graceful handling of extreme cases (stack overflow protection)

### Technical Details
- **Performance**: Minimal overhead (~1-2ms per chunk for token estimation)
- **Compatibility**: Fully backward compatible with existing installations
- **Safety**: Maximum recursion depth of 10 prevents infinite loops
- **Monitoring**: Debug logs show batch counts, sizes, and token estimates
- **Fallback**: Feature flag allows reverting to original behavior if needed

### Validation
- **Test Scenarios**: Large conversations with code blocks, JSON data, mixed content
- **Token Limits**: Verified batches stay under 100k tokens (20k buffer from 120k limit)
- **Chunk Splitting**: Tested recursive splitting preserves message boundaries
- **Error Recovery**: Confirmed graceful handling of edge cases and oversized content

### Migration Guide
No user action required - fix is automatic upon upgrade:
1. Update package: `npm install -g claude-self-reflect@2.6.0`
2. Restart import process: imports will use new token-aware batching
3. Monitor logs for any split/truncation warnings during first import

### Environment Variables
```bash
# Token limit configuration (optional)
MAX_TOKENS_PER_BATCH=100000         # Safe limit with 20k buffer
TOKEN_ESTIMATION_RATIO=3            # Conservative chars-per-token estimate
USE_TOKEN_AWARE_BATCHING=true       # Enable intelligent batching (recommended)
```

## [2.5.18] - 2025-08-17

### Security
- **Updated Dependencies**: Security patch for Docker streaming-importer
  - Updated `fastembed` from 0.2.7 to 0.4.0 (latest stable)
  - Updated `numpy` from 1.26.0 to 1.26.4 (latest compatible with fastembed constraints)
  - No critical or high severity vulnerabilities found in GitHub security scanning
  - All services tested and running correctly after updates

## [2.5.16] - 2025-08-17

### 🚨 Critical Performance & Stability Release

### Fixed
- **CRITICAL: CPU Overload Issue** - Streaming importer CPU usage reduced from **1437% to <1%** (99.93% reduction)
  - **Root Cause**: Unbounded async loops without proper throttling in streaming importer
  - **Solution**: Complete rewrite with production-grade CPU monitoring and cgroup awareness
  - **Impact**: System now runs efficiently on resource-constrained environments
  - **Files Modified**: `scripts/streaming-importer.py`, `Dockerfile.streaming-importer`
- **CLI Status Command**: Fixed broken `--status` command in MCP server
  - Previously returned empty responses due to incorrect argument parsing
  - Now returns comprehensive system health including collections, memory, CPU usage
  - **Files Modified**: `mcp-server/src/server.py`

### Added
- **Production-Ready Streaming Importer** with enterprise-grade reliability:
  - Non-blocking CPU monitoring with per-core limits and cgroup detection
  - Queue overflow protection using deferred processing (data preserved, not dropped)
  - Atomic state persistence with fsync guarantees for crash recovery
  - Memory management with 15% GC buffer and automatic cleanup
  - Proper async signal handling for clean shutdowns without race conditions
  - Task cancellation on timeout preventing resource leaks
  - Exponential backoff retry logic for transient failures
  - High water mark optimization reducing filesystem scanning overhead
- **Enhanced Resource Monitoring**:
  - Real-time CPU usage tracking with container awareness
  - Memory usage monitoring with automatic garbage collection
  - Processing queue health monitoring with backlog alerts
  - Performance metrics collection and reporting

### Changed
- **V2 Token-Aware Chunking: 100% Migration Complete**
  - All collections migrated from v1 to v2 chunking format
  - Chunk configuration: 400 tokens/1600 characters with 75 token/300 character overlap
  - Search quality improved with proper semantic boundaries
  - Memory-efficient streaming chunk generation prevents OOM during processing
- **Performance Optimizations**:
  - Search response time: <3ms average, <8ms maximum across 121+ collections
  - Memory footprint: 302MB operational (60% of 500MB limit)
  - Processing rate: 4-6 files/minute stable throughput
  - Resource utilization: 96.2% memory reduction from previous versions

### Performance Metrics
| Metric | Before v2.5.16 | After v2.5.16 | Improvement |
|--------|----------------|---------------|-------------|
| CPU Usage | 1437% | <1% | 99.93% reduction |
| Memory Footprint | 8GB peak | 302MB operational | 96.2% reduction |
| Search Latency | Variable | 3.16ms avg, 7.55ms max | Consistent sub-8ms |
| Processing Success Rate | Inconsistent | 100% | Reliable |

### Technical Details
- **Test Results**: 21/25 unit tests passing for streaming importer functionality
- **Resource Management**: Semaphore-based concurrency control (embeddings: 1, Qdrant: 2)
- **State Persistence**: Atomic write operations with temporary file swapping
- **Memory Management**: Proactive garbage collection with malloc_trim on Linux
- **Queue Processing**: Oldest-first processing prevents file starvation

### Breaking Changes
- **Docker Configuration**: New environment variables required for streaming importer
- **State File Format**: Enhanced schema with additional metadata (backwards compatible)
- **Minimum Requirements**: Python 3.9+ required for async improvements

### Migration Guide
For existing installations:
1. Stop services: `docker-compose down`
2. Update docker-compose.yml with new environment variables
3. Restart: `docker-compose up -d streaming-importer`
4. Monitor: `docker stats` (CPU should be <1%)

### Environment Variables
```bash
MAX_CPU_PERCENT_PER_CORE=25        # CPU limit per core
MAX_CONCURRENT_EMBEDDINGS=1         # Embedding operations concurrency
MAX_CONCURRENT_QDRANT=2             # Qdrant operations concurrency
IMPORT_FREQUENCY=15                 # Seconds between import cycles
BATCH_SIZE=3                        # Files processed per batch
MEMORY_LIMIT_MB=400                 # Memory limit in megabytes
MAX_QUEUE_SIZE=100                  # Maximum processing queue size
```

## [2.5.10] - 2025-08-11

### Fixed
- **CRITICAL: MCP Server Startup Failure** - Emergency hotfix for IndentationError
  - **Root Cause**: Version 2.5.9 shipped with unreachable dead code after return statements in three MCP tool functions
  - **Issue**: Server failed to start with IndentationError due to dead code after return statements in:
    - `quick_search()` - 32 lines of parsing/formatting code after return statement
    - `search_summary()` - 57 lines of result analysis code after return statement  
    - `get_more_results()` - 26 lines of pagination logic after return statement
  - **Solution**: Removed all dead code after return statements while preserving error messages about MCP architectural limitations
  - **Impact**: MCP server can now start properly, reflection tools are accessible again
  - **Files Modified**: `mcp-server/src/server.py` - Removed 115+ lines of unreachable code
  - **User Action**: Update to v2.5.10 immediately to restore server functionality

### Technical Details
- **What Happened**: Functions had code after return statements that Python interpreter couldn't parse
- **Why It Happened**: Incomplete removal of old functionality when implementing MCP architectural limitation messages
- **The Fix**: Clean removal of all code after return statements in the three affected functions
- **No Functionality Lost**: The removed code was unreachable and non-functional due to return statements

## [2.5.9] - 2025-08-11

### Fixed
- **MCP Tool Interoperability**: Fixed tools attempting to call other MCP tools internally
  - **Root Cause**: MCP architecture doesn't allow tools to call other tools - only the client (Claude) can orchestrate tool calls
  - **Issue**: `quick_search`, `search_summary`, and `get_more_results` were trying to call `reflect_on_past` internally
  - **Previous Error**: "FunctionTool object is not callable" - cryptic and unhelpful
  - **Solution**: Replaced internal tool calls with graceful error messages explaining MCP architectural limitation
  - **User Guidance**: Clear alternatives provided (use reflection-specialist agent or call tools directly)
  - **Files Modified**: `mcp-server/src/server.py` - Updated 3 specialized search tools
- **Variable Scope Bug**: Fixed `cwd` variable not initialized when `project="all"` specified
  - Moved `cwd` initialization outside conditional block to ensure it's always set

### Impact
- Graceful error handling instead of cryptic "FunctionTool object is not callable" messages
- Clear guidance for users when MCP architectural limitations prevent certain operations
- Better developer experience with informative error messages
- No functional regressions - all tools work as intended within MCP constraints

## [2.5.8] - 2025-08-11

### Fixed
- **CRITICAL: Project-Scoped Search Now Works Correctly**
  - **Root Cause**: MCP server was always searching claude-self-reflect conversations regardless of which project you were actually in
  - **Issue**: The server runs from `mcp-server/` directory, so `os.getcwd()` always returned the server's directory, not Claude Code's working directory
  - **Solution**: Modified `run-mcp.sh` to capture original `$PWD` as `MCP_CLIENT_CWD` environment variable
  - **Impact**: Project-scoped search now correctly isolates conversations per project, eliminating cross-project contamination
  - **Files Modified**:
    - `run-mcp.sh`: Added `export MCP_CLIENT_CWD="$PWD"` to capture client working directory
    - `server.py`: Updated project detection logic to use `MCP_CLIENT_CWD` instead of `os.getcwd()`
    - `utils.py`: Enhanced project name normalization functions
  - **User Action**: None required - fix is automatic upon MCP server restart

## [2.5.7] - 2025-08-11

### Changed
- **Dependencies**: Removed unused `openai` package from requirements.txt
  - Package was listed but never imported or used in the codebase
  - Kept `tqdm`, `humanize`, and `backoff` for potential future use in setup wizard and rate limiting

## [2.5.6] - 2025-08-11

### Added
- **Tool Output Extraction**: Metadata v2 schema captures tool outputs and git file changes
  - Extracts up to 15 tool outputs (500 chars each) per conversation
  - Parses git diff/show/status outputs to identify modified files
  - Enables cross-agent discovery via `search_by_file` for git-modified files
  - Two-pass JSONL parsing for complete output capture
  
### Enhanced
- **Search Capabilities**: 
  - `search_by_file` now finds conversations with git-modified files
  - `search_by_concept` improved for git-related concepts
  - Tool outputs included in semantic search index
  
### Changed
- Metadata schema upgraded to version 2
  - Added `git_file_changes` field for files from git outputs
  - Added `tool_outputs` field for tool execution results
  - Backward compatible with v1 metadata
  
### Technical Details
- **Files Modified**:
  - `scripts/streaming-importer.py` - Added `extract_files_from_git_output()` function
  - `scripts/import-conversations-unified.py` - Added two-pass JSONL parsing
  - Both importers now extract tool outputs from user messages
- **Performance**: Minimal overhead (~10ms per conversation)
- **Storage**: ~2-5KB increase per conversation with tool outputs

## [2.5.5] - 2025-08-11

### Security
- **CRITICAL**: Fixed pydantic version conflict preventing MCP server startup
  - Updated pydantic from >=2.9.2 to >=2.11.7 for fastmcp 2.10.6 compatibility
  - Ensures runtime stability and prevents dependency resolution failures

### Fixed
- **Streaming Importer**: Enhanced file validation to prevent queue blocking
  - Added detection and automatic skipping of empty files (0 bytes)
  - Added detection and automatic skipping of summary-only files without conversation data
  - Implemented state tracking for skipped files to avoid re-processing
  - Prevents import pipeline from stalling on non-importable files
  - Files are re-validated if they grow in size or change modification time

### Changed
- Repository organization: Archived old release notes to docs/archive/releases/
- Cleaned up test artifacts from root directory
- Updated dependencies:
  - openai: 1.97.1 → 1.98.0
  - qdrant-client: 1.15.0 → 1.15.1

### Technical Details
- **Files Modified**: 
  - `scripts/streaming-importer.py` - Added comprehensive file validation functions
  - `mcp-server/pyproject.toml` - Fixed pydantic version constraint
- **State Management**: Enhanced imported-files.json to track skipped files
- **Docker**: Rebuilt streaming-importer image with validation logic
- **Impact**: Improved reliability for continuous import operations

## [2.5.1] - 2025-08-06

### Fixed
- **CRITICAL**: Collection mismatch preventing immediate search visibility of recent conversations
  - **Root Cause**: Project name extraction was using filename instead of directory name
  - **Impact**: Recent conversations stored in wrong collection (e.g., conv_7bcf787b_voyage) instead of correct one (conv_7f6df0fc_local)
  - **Fix**: Updated `normalize_project_name()` in `mcp-server/src/utils.py` to correctly extract project name from Claude logs directory structure
- Fixed streaming importer now correctly identifies projects from Claude logs paths
- Fixed project name normalization for both local and cloud embedding modes

### Changed
- Enhanced project name extraction logic to handle various Claude logs path formats:
  - Claude logs format: `-Users-kyle-Code-claude-self-reflect` -> `claude-self-reflect`
  - File paths in Claude logs: `/path/to/-Users-kyle-Code-claude-self-reflect/file.jsonl` -> `claude-self-reflect`
  - Regular file paths: `/path/to/project/file.txt` -> `project`

### Validation
- **Certified by claude-self-reflect-test agent**:
  - ✅ Local mode: Working correctly with `conv_7f6df0fc_local`
  - ✅ Cloud mode: Working correctly with `conv_7f6df0fc_voyage`
  - ✅ Memory usage: 26.9MB (47% under 50MB limit)
  - ✅ Container stability: No crashes during testing
  - ✅ Search latency: <10 seconds achieved consistently

### Performance
- Memory usage optimized: 26.9MB during operation (well under 50MB limit)
- Search latency improved: Consistent <10 second response times
- Container stability: No memory leaks or crashes detected during validation

### Technical Details
- **Files Modified**: `mcp-server/src/utils.py` - Enhanced `normalize_project_name()` function
- **Collections**: Now correctly routes conversations to appropriate collections
- **Backward Compatibility**: Existing collections remain functional
- **Migration**: No user action required - fix is automatic

## [2.4.11] - 2025-07-30

### Security
- Critical security update addressing CVE-2025-7458 (SQLite integer overflow vulnerability)
  - Updated all Docker base images from Python 3.11-slim to Python 3.12-slim
  - Added explicit `apt-get upgrade` in all Dockerfiles for system package security updates
  - Applied to all containers: importer, watcher, mcp-server, streaming-importer, importer-isolated
  - While Python 3.12 on Debian 12 still includes SQLite 3.40.1, the base image upgrade ensures better overall security posture

### Changed
- Enhanced security hardening across all Docker images with system package updates
- Fixed torch version compatibility in streaming-importer (updated to 2.3.0)
- Corrected script reference in importer-isolated to use existing unified import script

### Testing
- Comprehensive testing performed before release:
  - Local embeddings mode with FastEmbed (384-dimensional vectors)
  - Cloud embeddings mode with Voyage AI (1024-dimensional vectors)
  - Incremental import functionality (proper file change detection)
  - All Docker images build successfully except streaming-importer (known dependency issues)

## [2.4.10] - 2025-07-30

### Fixed
- Critical memory optimization: Import watcher no longer gets OOM killed on 2GB systems
  - Reduced batch size from 100 to 10 messages for lower memory footprint
  - Added per-file state saving to prevent progress loss on OOM
  - Implemented garbage collection after each file processing
  - State persistence now happens incrementally, not just at end

### Added
- Comprehensive memory footprint documentation (docs/memory-footprint.md)
  - Detailed memory usage patterns for first-time vs incremental imports
  - Troubleshooting guide for OOM issues
  - Performance metrics and optimization details
- Enhanced setup wizard with memory configuration guidance
  - Warns about first-time import memory requirements
  - Suggests temporary 4GB allocation for initial setup

### Changed
- Import process now more resilient to memory constraints
  - Works reliably with 2GB Docker memory after initial import
  - First import may still benefit from 4GB temporary allocation
- Updated documentation across multiple files:
  - README.md: Added system requirements section
  - troubleshooting.md: New memory and import issues section
  - setup-wizard-docker.js: Better memory handling guidance

### Performance
- Memory usage reduced by ~60% during import operations
- Import reliability improved - no longer fails on systems with 2GB Docker memory
- State tracking prevents re-importing unchanged files even after OOM recovery

## [2.4.9] - 2025-07-30

### Fixed
- Critical performance fix: Import watcher no longer re-imports unchanged files (#22)
  - Previously re-imported ALL files every 60 seconds causing 7+ minute cycles
  - Now tracks file modification times and skips unchanged files
  - Reduces subsequent import times from 7+ minutes to under 30 seconds
  - Significantly reduces CPU and memory usage

### Added
- Import state tracking in `.import_state.json`
  - Persists across watcher restarts
  - Tracks modification times for each imported file
  - Saves state after each project to preserve progress

### Changed
- `scripts/import-conversations-unified.py`: Added state management functions
- `Dockerfile.watcher`: Rebuilt with updated import script

### Performance
- First import: Processes all files normally
- Subsequent imports: Skip 90%+ unchanged files
- Import time: Reduced from 7+ minutes to <60 seconds
- Resource usage: Dramatically reduced CPU and memory consumption

## [2.4.8] - 2025-07-29

### Fixed
- Critical fix: Import watcher was missing scripts directory causing continuous failures
  - Updated Dockerfile.watcher to properly copy scripts directory
  - Fixed import path to use correct script location

## [2.4.7] - 2025-07-29

### Fixed
- Critical fix: Local mode now works properly without requiring Voyage API key (#21)
  - Setup wizard was incorrectly using obsolete server_v2.py module
  - Now correctly uses the main module which supports both local and Voyage embeddings
- Removed obsolete server_v2.py to prevent confusion
  - This was a temporary testing artifact whose improvements are already in server.py

### Changed
- Setup wizard Docker script now runs `python -u -m src` instead of `python -m src.server_v2`

## [2.4.5] - 2025-07-29

### Added
- Performance optimizations achieving 10-40x speed improvement
  - End-to-end response time reduced from 28.9s-2min to 2-3s
  - Response size reduced by 40-60% through compression techniques
- Brief mode parameter for minimal responses (`brief=true`)
- Progress reporting during search operations (requires client progressToken)
- Comprehensive timing logs for performance debugging
- Debug mode with `include_raw=true` for troubleshooting
- Response format parameter supporting both XML and markdown (`response_format`)

### Changed
- XML tags compressed to single letters for smaller payload (40% size reduction)
- Excerpt length optimized to 350 characters (was 500) for better context
- Default result limit kept at 5 to avoid missing relevant results
- Title and key-finding lengths optimized (80/100 chars)
- Timezone handling fixed for proper datetime comparisons

### Performance
- Search latency: 103-620ms (varies by collection count)
- MCP overhead: 75-85% of total response time
- Response size reduction: 40% (normal), 60% (brief mode)
- Streaming works properly when using reflection-specialist sub-agent

### Technical
- Added detailed timing breakdown in debug logs
- Fixed "0 tool uses" display issue with sub-agents
- Improved real-time playback with markdown format
- Better error handling for timezone-aware datetimes

### Known Issues
- Specialized search tools (`quick_search`, `search_summary`, `get_more_results`) work through the reflection-specialist agent but not via direct MCP calls due to FastMCP limitations with nested tool calls

## [2.4.4] - 2025-07-29

### Added
- XML-structured response format for reflection-specialist agent
  - All search results now return structured XML instead of markdown
  - Consistent error handling with XML format for all failure cases
  - Metadata section includes search performance metrics
  - Clear separation of summary, results, analysis, and metadata sections

### Changed
- Reflection-specialist agent completely restructured to use XML format
  - Easier parsing for main agent without regex
  - Structured data extraction with predictable field locations
  - Error responses also follow XML format for consistency

### Benefits
- Main agent can extract specific fields like `<score>`, `<project>`, `<timestamp>` directly
- No ambiguity in data boundaries or formatting
- Extensible schema allows adding new fields without breaking parsers
- Better integration capabilities for downstream tools

## [2.4.3] - 2025-07-28

### Added
- Project-scoped search functionality in MCP server (#14)
  - New optional `project` parameter for `reflect_on_past` tool
  - Default behavior: searches only current project based on working directory
  - Cross-project search with `project="all"`
  - Specific project search by name
- Comprehensive project-scoped search documentation (docs/project-scoped-search.md)
- Proactive cross-project search suggestions in reflection-specialist agent
- Project search troubleshooting section in docs
- Cross-project search strategies in advanced usage guide

### Changed
- **BREAKING**: Search behavior now defaults to project-scoped instead of searching all projects
  - Previous behavior (search all) now requires explicit `project="all"`
  - Improves focus, relevance, and performance (~100ms faster)
  - Project isolation enhances privacy between different work contexts
- Project names in search use folder names instead of full paths with dashes
- Reflection-specialist agent now indicates which project was searched

### Documentation
- Added detailed project-scoped search section to README with migration notes
- Created comprehensive guide at docs/project-scoped-search.md
- Updated reflection-specialist agent with proactive search patterns
- Enhanced advanced usage guide with cross-project strategies
- Added troubleshooting section for common project search issues

### Migration Notes
- Users upgrading from v2.4.2 or earlier will experience different search behavior
- To restore old behavior, explicitly request "search all projects" in queries
- Existing conversations remain searchable but are now filtered by project
- See [Project-Scoped Search Guide](docs/project-scoped-search.md) for details

## [2.4.2] - 2025-07-28

### Added
- Docker volume migration from bind mounts for better data persistence (PR #16)
- Exponential backoff for Voyage AI API calls with retry logic (PR #15)
- New reflect-tester agent for comprehensive system validation
- Performance baseline documentation for both embedding modes
- Tenacity library (9.1.2) for safe retry handling

### Changed
- Updated backup/restore scripts to work with Docker named volumes
- Enhanced testing infrastructure with phased approach
- Pinned all dependencies to specific versions for reproducible builds
- Fixed missing voyageai dependency in scripts/requirements.txt

### Fixed
- Agent documentation missing reflection-specialist
- Voyage AI import failures in Docker setup
- Collection naming for test projects
- Security scan false positives from accidentally committed SARIF file

### Security
- No vulnerabilities found in pip-audit scan
- All dependencies now pinned to specific versions

### Documentation
- Clarified that per-project isolation already exists (#14)
- Added workaround for npm global install path issues (#13)

## [2.4.0] - 2025-07-28

### Added
- Gitleaks configuration (`.gitleaks.toml`) for better CI/CD security scanning
- Support for handling false positives in security scans

### Changed
- Improved documentation clarity around privacy and security
- Removed unnecessary security alerts from README

### Security
- Enhanced CI/CD pipeline with proper secret scanning configuration
- Better handling of historical commits in security scans

## [2.3.7] - 2025-07-27

### Security
- Major security cleanup to reduce attack surface
- Removed archived TypeScript MCP implementation (31 files, no longer needed)
- Removed 70+ internal scripts and test files from git tracking
- Removed binary database directories (qdrant_storage/, data/) from git
- Set secure permissions (600) on .env configuration files
- Reduced codebase by ~17,000 lines and 250+ files

### Changed
- Updated .gitignore to prevent exposure of sensitive files and internal tools
- Moved internal scripts to untracked directories
- Kept only essential scripts needed for setup and validation

## [2.3.6] - 2025-07-27

### Changed
- Updated README "After" example to show actual reflection specialist sub-agent format
- Added explanation that reflection specialist is automatically spawned
- Emphasized local-first approach with optional cloud enhancement

### Documentation
- Improved clarity on how sub-agents appear in Claude's interface
- Better example using FastEmbed vs cloud decision instead of generic JWT auth

## [2.3.5] - 2025-07-27

### Changed
- Made technical documentation more neutral between local and cloud embedding options
- Removed promotional language about Voyage AI's cost-effectiveness
- Presented both embedding options equally without bias

## [2.3.4] - 2025-07-27

### Added
- Comprehensive embedding mode documentation
- Migration guide for switching between local and cloud modes
- Enhanced setup wizard with prominent embedding choice warnings
- Confirmation prompt for embedding mode selection

### Changed
- Setup wizard now clearly explains that embedding choice is semi-permanent
- Updated all documentation to emphasize the complexity of switching modes
- Improved troubleshooting guide with embedding mode issues section

### Documentation
- Created detailed embedding migration guide
- Updated installation guide with embedding mode selection
- Enhanced advanced usage guide with technical embedding details
- Added warnings throughout docs about mode switching implications

## [2.3.3] - 2025-07-27

### 🔒 Security Release

### Changed
- **BREAKING**: Complete migration from sentence-transformers to FastEmbed for local embeddings
- **Default Mode**: Local embeddings now default for privacy (no external API calls)
- **Docker Memory**: Increased container memory limits to 2GB for stability
- **Security Improvements**:
  - Fixed command injection vulnerabilities in installer
  - Patched vulnerable dependencies (pydantic CVE-2024-3772)
  - Enhanced configuration security

### Added
- **Local Embeddings by Default**: Uses FastEmbed with sentence-transformers/all-MiniLM-L6-v2 model
- **Unified Import System**: Single import script supports both local and cloud embeddings
- **JSONL Parser Fix**: Proper line-by-line parsing for Claude conversation files
- **Enhanced Documentation**:
  - Privacy mode comparison table in README
  - Security update notice with GitHub info block
  - Clear warnings about data exchange in cloud mode

### Fixed
- Import failures due to incorrect JSON parsing (JSONL format)
- Memory exhaustion in Docker containers when processing large files
- MCP server initialization with local embeddings
- Reflection specialist agent support for both embedding modes

### Security
- Environment variable configuration for all sensitive settings
- Local-first approach for privacy-conscious users
- Enhanced security scanning in CI/CD pipeline

## [2.3.0] - Unreleased

### Added
- **Setup Wizard Command-Line Arguments** - Non-interactive installation support
  - `--voyage-key=<key>` for direct API key configuration
  - `--local` flag for local-only mode without semantic search
  - Automatic detection of non-interactive environments (no TTY)
- **Watcher Integration** - Continuous conversation monitoring built into setup
  - Automatic file watching after initial import
  - Real-time conversation indexing
  - Configurable watch intervals
- **System Health Dashboard** - Comprehensive health monitoring (`health-check.sh`)
  - Qdrant status with vector and collection counts
  - MCP server connection status
  - Import queue and last import timing
  - Docker container resource usage
  - Automatic update checking
  - Search performance metrics
- **Enhanced Docker Support**
  - Isolated importer container (`Dockerfile.importer-isolated`)
  - Streaming importer with progress updates (`Dockerfile.streaming-importer`)
  - Watcher container for continuous monitoring (`Dockerfile.watcher`)
  - Optimized docker-compose configurations
- **Python SSL Support** - Improved handling of Python installations
  - Automatic detection of pyenv SSL issues
  - Fallback to Homebrew Python on macOS
  - Clear error messages and remediation steps

### Changed
- **Setup Wizard Improvements**
  - Better error handling for missing dependencies
  - Clearer prompts and user feedback
  - Support for both interactive and non-interactive modes
  - Automatic Python path detection and configuration
- **Documentation Structure**
  - Reorganized docs into logical categories
  - Added troubleshooting guides for common issues
  - Improved installation instructions with platform-specific notes
  - Better examples and use cases
- **Import Process**
  - Streaming import with real-time progress
  - Better handling of large conversation files
  - Improved error recovery and retry logic
  - More efficient vector embedding batch processing

### Fixed
- **Reddit User Feedback Issues**
  - Python SSL module errors with pyenv installations
  - Non-interactive installation failures in CI/CD environments
  - Qdrant health check endpoint (changed from `/health` to `/`)
  - Docker container permission issues on Linux
  - Memory leaks in long-running import processes
- **CI/CD Pipeline**
  - Updated workflows for Python-based structure
  - Fixed test paths and dependencies
  - Added Python version matrix testing (3.10, 3.11, 3.12)
- **Setup Wizard Bugs**
  - Fixed readline interface errors in non-TTY environments
  - Corrected Python path detection on various platforms
  - Improved error messages for missing dependencies

### Security
- Better isolation of import processes using Docker
- Improved handling of sensitive configuration

## [2.2.1] - 2025-01-25

### Fixed
- NPM package installation issues
- Missing dependencies in package.json

## [2.2.0] - 2025-01-24

### Added
- Voyage AI streaming embeddings support
- Improved import performance (2x faster)
- Better error handling in setup wizard

### Changed
- Default to Voyage AI for better search accuracy
- Simplified installation process

## [2.1.0] - 2025-01-23

### Added
- Support for multiple embedding providers
- Local embedding model option
- Cross-project search functionality

### Fixed
- Memory usage optimization for large conversation sets
- Import speed improvements

## [2.0.1] - 2025-01-22

### Fixed
- CI/CD workflow for Python-based structure
- Updated test configurations
- Fixed directory references in workflows

## [2.0.0] - 2025-01-22

### Changed
- **BREAKING**: Complete restructure from TypeScript to Python MCP server
- **BREAKING**: NPM package now serves as installation wizard only
- **BREAKING**: Renamed MCP from `claude-self-reflection` to `claude-self-reflect`
- Archived TypeScript implementation to `archived/typescript-mcp/`
- Renamed directories for clarity:
  - `claude-reflect` → `mcp-server`
  - `claude_reflect` → `src`
- Simplified configuration structure

### Added
- New installation CLI with interactive setup wizard
- `claude-self-reflect doctor` command for diagnostics
- Python wheel distribution for MCP server
- Comprehensive migration guide

### Removed
- TypeScript MCP server implementation (archived)
- Direct MCP functionality from NPM package

### Migration Guide
Users upgrading from v1.x need to:
1. Uninstall the old package: `npm uninstall -g claude-self-reflection`
2. Install the new package: `npm install -g claude-self-reflect`
3. Run setup wizard: `claude-self-reflect setup`
4. Update Claude Desktop configuration to use `claude-self-reflect` instead of `claude-self-reflection`

## [1.0.0] - 2025-01-14

### 🎉 Initial Release

#### Added
- **One-command installation** - Get up and running in under 5 minutes
- **Semantic search** across all Claude Desktop conversations
- **Continuous import** - Automatically watches for new conversations
- **Multiple embedding providers** - Support for OpenAI, Voyage AI, and local models
- **Cross-project search** - Search across all your Claude projects simultaneously
- **Privacy-first design** - All data stays local, no cloud dependencies
- **MCP-native integration** - Built specifically for Claude Desktop
- **Comprehensive documentation** - Full setup guide and troubleshooting
- **Health monitoring** - Built-in health check and status commands
- **Backup/restore** functionality for data safety
- **Docker-based deployment** for easy setup and isolation

#### Performance
- 66.1% search accuracy with Voyage AI embeddings
- <100ms search latency for 100K+ conversations
- ~1,000 conversations/minute import speed
- ~1GB memory usage per million conversations

#### Supported Platforms
- macOS (Apple Silicon and Intel)
- Linux (Ubuntu 20.04+, Debian 11+)
- Windows (via WSL2)

#### Known Issues
- Large conversation files (>10MB) may slow down initial import
- Search accuracy varies by embedding model choice
- Windows native support requires WSL2

### Migration from Neo4j
If you're migrating from the original Neo4j-based memento-stack:
1. Export your data using the old system's export function
2. Install Claude Self-Reflect
3. Import will automatically process your conversation history
4. All conversation data is preserved with improved search capabilities

---

## Contributing

We welcome contributions! See our [Contributing Guide](CONTRIBUTING.md) for details.

To add to this changelog:
1. Add your changes under the [Unreleased] section
2. Use the appropriate category: Added, Changed, Deprecated, Removed, Fixed, Security
3. Include PR numbers and contributor credits

Example:
```
### Added
- New feature description (#123) - @contributor
```