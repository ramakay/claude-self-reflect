# T3 Results: Cross-Source Multi-Hop Benchmark

**Run date:** 2026-07-27. Pre-registration: `saga-t3-preregistration.md`
(committed 2302de0 with sealed 27-question roster before any arm ran).
Corpus snapshot sha256 `1d2b9923…`; question seal `1b331d35…`;
mechanical roster 396 receipt-class pairs (sha `5e03eb25…`).

## Headline (pre-committed interpretation applies)

**Gate M: FAIL. Gate D: NULL. Reinstatement's advantage over flat kNN is not
supported on this corpus.** Per the pre-registered failure interpretation, the
paper's supported contributions reduce to the receipts layer, the resolution
ledger (hygiene/demotion), and provenance-aware rerank — not the reinstatement
walk itself.

Secondary but decisive: **memory itself dominates** — the no-memory arm scored
0/27 consensus-correct. Every correct answer in the benchmark required
retrieved context. The corpus and retrieval layer work; the five-step walk does
not earn its cost over one-shot kNN+FTS on this corpus.

## Gate M — mechanical set (396 receipt-gold commit-message queries)

| arm | recall@5 of authoring session |
|---|---|
| R (reinstatement) | 216/396 = 0.545 |
| K (kNN + FTS RRF) | **310/396 = 0.783** |

Discordant pairs: R-only 13, K-only 107. Two-sided exact sign test p ≈ 0.
**K > R, decisively.** Mechanism: commit messages share verbatim tokens with
the authoring session's transcript; the FTS component (granted to K by the
pre-registration's fairness clause) matches them exactly, while the
reinstatement walk dilutes exact-token evidence into semantic neighborhoods.
Receipt lookup is a single-hop exact-key task; deliberation machinery is
overhead there. This directly motivates the chunk↔commit typed-edge extractor
(v9.5): what won this gate, productized as an O(1) join.

## Gate D — deep set (27 sealed questions × 3 arms, cross-family judging)

Answers: sonnet via `claude -p` in a clean-room environment (see Deviations),
identical prompt scaffold per arm; R/K contexts from the frozen snapshot,
arm N received none. Judges: Codex and Grok CLIs (no Claude-family judge),
blinded X/Y/Z per task (md5-shuffled), grading strictly against sealed
ground-truth keys. Consensus = both judges agree; splits excluded (6 of 81).

| subset | nomem | kNN | R | discordant | p |
|---|---|---|---|---|---|
| multi-hop (primary, n=14) | 0/14 | 6/13 | 3/13 | R-only 0, K-only 2 | 0.50 |
| full set (n=27) | 0/27 | 13/24 | 10/24 | R-only 1, K-only 2 | 1.00 |

**Gate D: NULL** — no significant difference between R and K on the multi-hop
subset (the pre-scoped claim), direction mildly favoring K. This replicates
the earlier 16-task benchmark's tie (27/36 = 27/36) on an independent,
git-grounded, cross-source question set with external cross-family judges.

## Deviations (all recorded before results were interpreted)

1. Pre-arm amendments 1–5 (K-arm RRF mechanics, registry symmetry, ledger
   rendering, mechanical roster freeze, snapshot identity) — committed before
   any arm ran; see preregistration.
2. **Answer-generation contamination incident (caught and corrected before any
   judging):** the first 81-dialogue generation ran `claude -p` with default
   settings; the operator's auto-memory file loaded into all three arms, and
   for CSR-corpus questions that file contains answer material (the nomem arm
   was observed citing a "memory note" containing the ground truth of
   question 1). The entire batch was deleted and regenerated in a clean-room
   working directory with settings sources cut and MCP disabled; a probe
   confirmed no styling hooks, memory, or project context loaded. No judging
   was performed on the contaminated batch. This is a third observed
   self-contamination vector (assistant-side memory injection), alongside the
   two documented in the paper (corpus self-indexing; session artifacts).
3. Grok judge initially emitted one malformed grading file (t3-26); the
   judge-driver re-ran it and the final parse recovered all 81/81 verdicts.
4. Judge split rate: 6/81 label-verdicts (7.4%) excluded as unresolved.

## What survives, what dies

- **Dies:** the claim that the reinstatement walk beats flat retrieval on
  multi-hop provenance questions — not supported at either gate, on a corpus
  purpose-built to contain genuine cross-source chains.
- **Survives, strengthened:** (a) memory vs no-memory is not close (0/27 vs
  13/24); (b) receipts as settlement layer (T1 0.913 precision, all failures
  receipt-less; 423/529 commits receipt-linked to authoring sessions);
  (c) resolution-ledger demotion (T2: write-target staleness 100%→0%,
  reversible, no fresh-content suppression); (d) exact-key structural joins
  as the cheapest high-precision provenance mechanism (Gate M's winner).
- **Next scientific step:** ablate *which component* of K's win is FTS vs
  vector (exploratory, post-hoc, labeled as such), and test whether typed
  receipt edges (chunk↔commit) close the gap R couldn't.
## Amendment A — contamination vector 4 and clean re-run (recorded after results were committed)

**Discovery.** A hook-recursion bug (fixed in PR #264) had caused ~4,800 nested
`claude -p` extractor/narrative spawns to write transcripts that the import
watcher indexed as conversations. The frozen snapshot (`1d2b9923…`) contained
**4,346 such conversations — 84% of its 5,187 conversations, 38% of its
154,389 chunks**. The pre-registered freeze exclusion (119 conversations)
missed them because they predate the 2026-07-27 cutoff. This is a fourth
self-contamination vector (assistant-side hook recursion), alongside the three
already documented.

**Impact measurement (original run).** Garbage occupied 9.5% of R's Gate M
top-5 slots vs 5.1% of K's; Gate D contexts carried ~14% garbage items in both
arms (7-8 of 27 questions affected each arm) — symmetric.

**Clean re-run.** Snapshot scrubbed of all 4,346 spawn conversations
(classifier: first chunk = extractor prompt; 0 overlap with mechanical gold;
scrubbed sha `5af69d81…`, 841 conversations / 97,861 chunks, residual 0).
Re-run scope: Gate M fully re-run, and Gate D's K and R answers regenerated
from scrubbed contexts; the no-memory answers were reused (they consume no
retrieved context, so the scrub cannot affect them):

| Gate M recall@5 | R | K | discordant | p |
|---|---|---|---|---|
| as-published (polluted) | 0.545 | 0.783 | 13/107 | ≈1.5e-19 |
| clean re-run | 0.581 | 0.813 | 11/103 | ≈6.9e-20 |

| Gate D (clean; fresh K/R answers from scrubbed contexts, nomem reused; grok+codex consensus, 7 splits) | nomem | K | R | discordant | p |
|---|---|---|---|---|---|
| multi-hop (primary, n=14) | 0/14 | 4/12 | 3/13 | R-0 / K-2 | 0.50 |
| full set (n=27) | 0/27 | 11/22 | 10/25 | R-0 / K-4 | 0.125 |

**Both verdicts replicate on the clean corpus.** Gate M FAIL stands; Gate D
NULL stands. Contamination depressed absolute scores symmetrically but changed
no conclusion.

## Amendment B — mechanism correction (FTS-vs-vector ablation, post-hoc exploratory)

The published mechanism sentence ("the FTS component matches them exactly,
while the reinstatement walk dilutes exact-token evidence") is **wrong** per
the ablation on the clean snapshot:

| arm | recall@5 |
|---|---|
| K fused (vector+FTS RRF) | 0.813 |
| K vector-only | 0.697 |
| R (walk) | 0.581 |
| K FTS-only | 0.162 |

FTS alone is weak; the vector channel does the heavy lifting and RRF fusion
adds ~12 points. The decisive fact is that **the walk scores 12 points below
its own seed channel** (0.581 vs 0.697). Traced causes in
`search/reinstatement.rs`: (1) `prefer_non_echo_seeds` deprioritizes seeds
containing the query verbatim — on receipt lookup the verbatim-containing
chunk IS the gold, so hop-2 spreads from wrong sessions (63/103 failures: gold
absent from top-10); (2) `W_QUERY_ECHO` (−0.35) + scaffold (−0.30) demotions
stack on receipt-shaped chunks (40/103 failures: gold at rank 6-10). The
observer-effect defenses, correct for conversational recall, invert into
anti-lookup behavior on exact-key tasks. This sharpens, rather than weakens,
the receipts conclusion: verbatim/receipt signals must be consumed as typed
keys (chunk↔commit edges, v9.5 task #5), not left to semantic machinery that
is tuned to distrust them.

Additional scoping note for Gate D: both arms' context files render 200-char
excerpts per item; multi-hop evidence chains are frequently truncated for both
arms, which bounds absolute Gate D scores independently of the R/K comparison.
