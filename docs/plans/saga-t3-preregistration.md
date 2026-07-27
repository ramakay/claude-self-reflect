# T3 Pre-Registration: Cross-Source Multi-Hop Benchmark

**Status: REGISTERED — committed before any benchmark arm has run.**
Registered: 2026-07-27. No arm may execute before this document's commit lands on the branch.

## Hypothesis

Reinstatement recall (R) outperforms flat kNN (K) on provenance questions whose
evidence spans corpus sources (plan → conversation → task/artifact), when both
arms operate over the identical merged multi-source corpus. The prior benchmark
(saga-relitigation-results.md) tied R = K = 27/36 on predominantly single-hop
questions; the surviving claim is scoped to multi-hop, and this benchmark tests
exactly that scope.

## Arms

1. **N (no-memory)**: model answers from parametric knowledge + question text only.
2. **K (flat kNN)**: one-shot embedding retrieval over the merged corpus
   (conversations + plans + registry-spine metadata), same k budget as R,
   same reranker settings where applicable. K retains substring/FTS access —
   commit hashes in chunk text are searchable text for K; only *typed edge
   traversal* is exclusive to R.
3. **R (reinstatement)**: `csr_why` five-step walk (kNN seed → context blend →
   code-graph spread → episode hop → provenance rerank) over the same corpus,
   with the resolution ledger loaded (T2 verdicts written before any arm runs).

## Materials

- **Deep set**: saga-t3-final-set.md — 27 questions (filter pass over 68
  git-grounded candidates; 14 multi-hop, 9 abandonment/supersession, corpus
  split CSR 11 / Anukriti 16; drop log in saga-t3-drops.md). Roster frozen
  in the same commit as this document; no post-hoc exclusion.
  Generation was blind to the retrieval system: question miners were forbidden
  CSR MCP tools and the reflection DB; ground truth derives from git history,
  raw plan files, and task lifecycles only.
- **Mechanical set**: pairs_typed.csv — 529 commit-message queries; gold =
  conversation(s) whose transcript contains the commit hash, typed
  receipt-class (423, hash inside git tool_result — authoring session) vs
  citation-class (86). Primary mechanical gold: receipt-class conversations.

## Corpus freeze

One corpus snapshot, constructed once, used by all arms:

- Includes all conversations imported as of 2026-07-27 **except** any
  conversation whose session start postdates 2026-07-27 00:00 local in the
  claude-self-reflect project scope (excludes this benchmark's own construction
  sessions — the self-retrieval contamination class documented in the paper's
  observer-effect section; the graded_sheet.csv artifact has already been
  observed surfacing as a search hit).
- Includes plan chunks (48) and session_registry (640 sessions) as populated
  2026-07-27.
- Snapshot hash recorded at freeze time; all arms run against it.

## Metrics and gates

**Mechanical set** (scored automatically, no judge):
- Primary: recall@5 of receipt-class gold conversation (hit = any top-5 result
  from a gold conversation).
- Gate M: R > K by sign test over per-query hit/miss discordant pairs, p < 0.05.

**Deep set** (blind-judged):
- Each arm's answer graded against ground_truth_key + grading_note by
  cross-family judges (Codex and Grok lanes; no Claude-family judge), blinded
  to arm identity, answers presented in shuffled order per question.
- Primary: correct/incorrect per question per arm; Gate D: R > K by sign test
  over discordant pairs, p < 0.05, computed over the multi-hop subset
  (the pre-scoped claim); full-set result reported regardless.

**Reporting commitments** (regardless of outcome):
- Both gates reported with exact counts; a null or negative result is published
  in the paper addendum with the same prominence as a positive one.
- No post-hoc question exclusion: the frozen roster is final once committed.
  Judge disagreements resolved by majority; unresolvable rows reported as such.
- Any deviation from this protocol is documented in the results file as a
  deviation, with rationale, before results are interpreted.

## Order of operations (enforced)

1. T2 verdict writes complete (ledger loaded) — done before freeze.
2. final_set.md frozen and committed together with this document.
3. Corpus snapshot frozen; hash recorded.
4. Arms run (N, K, R) headless against snapshot.
5. Mechanical scoring; judging packets built; cross-family judging.
6. Results file: saga-t3-results.md.

## Failure interpretation (pre-committed)

- Gate M fails, Gate D fails: reinstatement's multi-hop advantage is not
  supported on this corpus; the paper's claim reduces to the rerank/hygiene
  contributions and the receipts layer. This is reported as the headline.
- Gate M passes, Gate D fails (or vice versa): mixed result reported as such;
  no cherry-picking the passing gate as primary. Both were primary.
- Gates pass: claim stands as scoped — multi-hop provenance, single-operator
  corpus, generalization still unproven.
