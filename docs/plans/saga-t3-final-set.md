# T3 Multi-Hop Retrieval Eval — Final Set (27 questions)

Merged from `candidates.md` (38, CSR/csr-engine repo) and `candidates_private-app.md` (30,
private-app family), filtered against `pairs_typed.csv` (529 mechanical commit-pairs) for
redundancy. See `drops.md` for every excluded/re-typed candidate and why.

Corpus split: CSR 11/27 (41%), private-app 16/27 (59%). Multi-hop (hop=multi): 14/27, of
which 12 are D-type or audited-multi-hop-C-type (meets the ≥12 D+C floor). Abandonment/
supersession-themed: 9/27 (meets ≥6 floor).

Numbering below is fresh (1-27); each entry's original candidate number is noted for
traceability back to the source files.

---

## CSR / csr-engine (11)

### 1 (orig #1)
**Q:** Why did the reflection system gain a way for an agent to explicitly mark a recalled item as resolved, rather than relying on the assistant's prose saying "already shipped"?
**Type:** A · **Hop:** single · **Corpus:** CSR
**Ground truth key:** Plan `crispy-coalescing-goblet.md` §Context: dogfooding showed 4/5 recalled "queued" items were already shipped — the verifying agent's verdict existed only in-session, then evaporated as prose. Shipped as commit `8927f77` (2026-07-24) `feat(mcp): resolution ledger — csr_resolve tool, verdict annotation, page demotion (#251)`.
**Grading note:** Correct answer names the dogfooding observation (verdict existing only as prose, never persisted) as the trigger, and connects it to `csr_resolve`/the resolution ledger as the fix. Wrong answer: attributes the feature to a generic "better UX" motivation without the dogfooding-observation specifics, or invents a different trigger event.

### 2 (orig #7)
**Q:** Why was a Spearman-correlation check against the sealed E2 grades made a required, non-optional gate in the ratification work, rather than something to check after building everything?
**Type:** A · **Hop:** single · **Corpus:** CSR
**Ground truth key:** Plan `indexed-waddling-shamir.md` §Stage A: "Gate A′ (hard stop, inline): correlate backfill scores against sealed E2 grades ... No correlation → program halts, user decides. This is the advisor's deciding-risk tripwire."
**Grading note:** Correct answer names it as a pre-registered halt condition, meant to avoid crediting a signal later proven flat, echoed later in the actual halt doc (`30d7c66`). Wrong answer: describes it as a nice-to-have sanity check added after the fact, or omits the "advisor's deciding risk" framing entirely.

### 3 (orig #9)
**Q:** Plan work proposed keeping the `csr_resolve` tool out of the set of tools a background agent can invoke on its own — did that restriction make it into the shipped tool?
**Type:** B · **Hop:** single · **Corpus:** CSR
**Ground truth key:** Plan `crispy-coalescing-goblet.md` §3: "NOT added to `TASKABLE_TOOLS` (tasks.rs test asserts write tools excluded)." Verified in shipped `csr-engine/src/mcp/tasks.rs` / commit `8927f77`.
**Grading note:** Correct answer: yes, `csr_resolve` was excluded from `TASKABLE_TOOLS`, and a test asserts this. Wrong answer: claims `csr_resolve` is taskable/agent-invokable, or is unsure.

### 4 (orig #10)
**Q:** The ratification-memory plan called for a two-arm re-litigation benchmark comparing a ratification-weighted memory arm against a similarity-only control — did that specific benchmark ever get run?
**Type:** C · **Hop:** single · **Corpus:** CSR
**Ground truth key:** Plan `indexed-waddling-shamir.md` §Stage D (M vs C arms, 10 sealed tasks). Verified absence via commit `30d7c66` (2026-07-20): "Stage D benchmark not run per the advisor's deciding risk." No `examples/saga_relitigation.rs` file was ever created. The benchmark that *did* ship later (`1adb5c1`, `94c6f3f`) is a differently-designed three-arm (R/K/N) benchmark from a separate track, not this one.
**Grading note:** Correct answer must say this exact M-vs-C, 10-task benchmark never ran (halted before Stage D), and must not conflate it with the later, differently-designed 16-task R/K/N re-litigation benchmark that did run. Wrong answer: says the benchmark ran, citing the R/K/N results as if they were this benchmark.

### 5 (orig #11)
**Q:** Did the "operator-turn-prioritized" digest extractor for ratification scoring actually prioritize operator turns from the moment it shipped?
**Type:** C · **Hop:** single · **Corpus:** CSR
**Ground truth key:** Commit `05a1823` (2026-07-19) claimed the v2 extractor was operator-turn-prioritized. Correction in `41ff6f5` (2026-07-20): `get_chunks_by_ids` reconstructed chunks with a hardcoded `Speaker::ToolResult` author, so v2 silently fell back to head/tail sampling — its only real change was the rebalanced prompt. Fixed in PR #246 (`get_chunks_by_ids_with_provenance`).
**Grading note:** Correct answer says no — v2 silently never applied the operator-turn filter due to a hardcoded-author bug; it only genuinely activated in the v3 rerun after the fix. Wrong answer: confidently states v2 worked as advertised from the start.

### 6 (orig #14)
**Q:** Does the `supersedes` field on a memory chunk's provenance — the one the reranker gives a score boost to — actually get set to something other than empty by any of the live import or daemon pipelines?
**Type:** C · **Hop:** single · **Corpus:** CSR
**Ground truth key:** `grep -rn "supersedes" csr-engine/src` shows every live call site (`engine.rs:297`, `search/reinstatement.rs:688`, `daemon/ratification.rs:671,681`, `import/plans.rs:408`) hardcodes `supersedes: None`. The only non-`None` values appear in unit-test fixtures (`storage/mod.rs`, `search/rerank.rs` tests, `eval/continuity.rs`).
**Grading note:** Correct answer: no, in production code paths it is always `None`; only test/eval fixtures populate it. Wrong answer: claims it's actively used in real search to demote superseded memories.

### 7 (orig #16)
**Q:** Did the final blinded benchmark grading show reinstatement-based memory retrieval beating plain kNN similarity search, or something more modest?
**Type:** C · **Hop:** single · **Corpus:** CSR
**Ground truth key:** Commit `94c6f3f` (2026-07-24): "reason quality 27/36 (R) = 27/36 (K) > 24/36 (N)... reinstatement gains no separation over plain kNN in the single-hop regime — invention's claim stays localized to multi-hop provenance."
**Grading note:** Correct answer: it tied kNN (27=27), both beating no-memory (24); reinstatement's distinct value was later scoped to multi-hop only, not a general win over kNN. Wrong answer: claims reinstatement beat kNN outright.

### 8 (orig #17)
**Q:** Did the CSR paper keep an early framing that reinstatement recall improves ranking, or was that language walked back after the re-litigation results came in?
**Type:** C · **Hop:** single · **Corpus:** CSR
**Ground truth key:** Commit `af47eec` (2026-07-24) `docs(paper): fold in re-litigation benchmark, resolution ledger, silently-inert finding` — body: "conclusion adds 'Measured as a tie' scoping reinstatement to multi-hop."
**Grading note:** Correct answer identifies the walk-back to a "measured as a tie" / multi-hop-only claim. Wrong answer: says the paper kept its original stronger ranking-improvement claim unchanged.

### 9 (orig #20)
**Q:** As of the current unmerged multi-source-corpus branch, has the "task-derived resolution proposals" feature (auto-suggesting resolutions from completed tasks) reached the main branch yet?
**Type:** B · **Hop:** single · **Corpus:** CSR
**Ground truth key:** Commit `a9b6e86` (2026-07-27) `feat(hooks): task-derived resolution proposals + task-dir telemetry`, only on `feat/multi-source-corpus-v9.4` (`git merge-base --is-ancestor a9b6e86 origin/main` → not an ancestor, i.e. absent from main as of 2026-07-27).
**Grading note:** Correct answer: no, not yet merged to main — still on the feature branch awaiting PR/merge. Wrong answer: assumes it shipped because it's committed and version-bumped locally.

### 10 (orig #28, re-typed C, hop downgraded from claimed-multi to single — see drops.md)
**Q:** The resolution-ledger plan says its branch would be "stacked on `result-hygiene`" with the PR base retargeted to `main` only after PR #1 merged, due to a GitHub outage blocking PR creation. Did the merge actually happen in that order?
**Type:** C · **Hop:** single (audited down from the original D/multi-hop claim) · **Corpus:** CSR
**Ground truth key:** Plan `crispy-coalescing-goblet.md` §Isolation. Git evidence: `8927f77`'s parent is `7f0f02f` (the result-hygiene-track commit, merged as PR #250), and `8927f77` itself merged as PR #251 — consistent with the stacked-then-retargeted order.
**Grading note:** Git alone confirms the *sequencing* (#250 before #251, correct parent chain) — this part is single-hop and fully checkable from git. It cannot confirm the GitHub-outage *reason* was real rather than a contemporaneous belief; that claim has no corroborating source among git/plan/task at all (not even a second one to combine — hence the original "multi-hop" framing was inflated, downgraded here). Correct answer: confirms sequencing, and explicitly flags the outage-reason as unverifiable from these sources. Wrong answer: asserts the GitHub-outage reason as established fact, or gets the merge order backwards.

### 11 (orig #30 — reworded to remove task-ID leakage)
**Q:** A task recorded as "Stage D: re-litigation benchmark (seal, harness, run, grade)" shows a terminal status of `deleted`, with no completion timestamp ever recorded. What happened to that task, and does it match the plan's account of why Stage D never ran?
**Type:** D · **Hop:** multi · **Corpus:** CSR
**Ground truth key:** lifecycles.csv row for task `73f0fb7d-f099-4777-b364-d96cc954e598` (created 2026-07-19T01:36:49Z, `final_status=deleted`, no `completed_at`). Cross-check: commit `30d7c66` (2026-07-20) states Stage D was not run "per the advisor's deciding risk," consistent with the task being abandoned rather than completed.
**Grading note:** Correct answer connects the `deleted` task status (task-registry source) to the documented halt reason (git-commit source) — genuinely needs both sources, since neither alone states the causal link explicitly. Whether the task was deleted *because of* the halt (vs. an unrelated bookkeeping reason) is the part git/docs can only support circumstantially, not first-person. Wrong answer: treats the deleted status as unexplained/random, or invents a causal statement no source actually makes.

---

## private-app family (16)

### 12 (orig #39)
**Q:** [private repository task]
**Type:** A · **Hop:** single · **Corpus:** private-app
**Ground truth key:** [private repository task: answer withheld]
**Grading note:** [withheld]

### 13 (orig #42)
**Q:** [private repository task]
**Type:** A · **Hop:** single · **Corpus:** private-app
**Ground truth key:** [private repository task: answer withheld]
**Grading note:** [withheld]

### 14 (orig #46)
**Q:** [private repository task]
**Type:** A · **Hop:** single · **Corpus:** private-app
**Ground truth key:** [private repository task: answer withheld]
**Grading note:** [withheld]

### 15 (orig #47)
**Q:** [private repository task]
**Type:** B · **Hop:** multi · **Corpus:** private-app
**Ground truth key:** [private repository task: answer withheld]
**Grading note:** [withheld]

### 16 (orig #49)
**Q:** [private repository task]
**Type:** B · **Hop:** multi · **Corpus:** private-app
**Ground truth key:** [private repository task: answer withheld]
**Grading note:** [withheld]

### 17 (orig #54)
**Q:** [private repository task]
**Type:** C · **Hop:** multi · **Corpus:** private-app
**Ground truth key:** [private repository task: answer withheld]
**Grading note:** [withheld]

### 18 (orig #55)
**Q:** [private repository task]
**Type:** C · **Hop:** multi · **Corpus:** private-app
**Ground truth key:** [private repository task: answer withheld]
**Grading note:** [withheld]

### 19 (orig #56)
**Q:** [private repository task]
**Type:** C · **Hop:** multi · **Corpus:** private-app
**Ground truth key:** [private repository task: answer withheld]
**Grading note:** [withheld]

### 20 (orig #57)
**Q:** [private repository task]
**Type:** C · **Hop:** multi · **Corpus:** private-app
**Ground truth key:** [private repository task: answer withheld]
**Grading note:** [withheld]

### 21 (orig #59)
**Q:** [private repository task]
**Type:** D · **Hop:** multi · **Corpus:** private-app
**Ground truth key:** [private repository task: answer withheld]
**Grading note:** [withheld]

### 22 (orig #60)
**Q:** [private repository task]
**Type:** D · **Hop:** multi · **Corpus:** private-app
**Ground truth key:** [private repository task: answer withheld]
**Grading note:** [withheld]

### 23 (orig #61)
**Q:** [private repository task]
**Type:** D · **Hop:** multi · **Corpus:** private-app
**Ground truth key:** [private repository task: answer withheld]
**Grading note:** [withheld]

### 24 (orig #62)
**Q:** [private repository task]
**Type:** D · **Hop:** multi · **Corpus:** private-app
**Ground truth key:** [private repository task: answer withheld]
**Grading note:** [withheld]

### 25 (orig #63)
**Q:** [private repository task]
**Type:** D · **Hop:** multi · **Corpus:** private-app
**Ground truth key:** [private repository task: answer withheld]
**Grading note:** [withheld]

### 26 (orig #64)
**Q:** [private repository task]
**Type:** D · **Hop:** multi · **Corpus:** private-app
**Ground truth key:** [private repository task: answer withheld]
**Grading note:** [withheld]

### 27 (orig #67)
**Q:** [private repository task]
**Type:** D · **Hop:** multi · **Corpus:** private-app
**Ground truth key:** [private repository task: answer withheld]
**Grading note:** [withheld]

---

File: /tmp/scratch/t3-eval/final_set.md
