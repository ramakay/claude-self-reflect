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
