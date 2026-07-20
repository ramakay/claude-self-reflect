# Saga Eval Kit — Phase 2 Experiment Harness

Scripts, prompts, specs, and metadata for the three Phase-2 experiments behind the
saga/reinstatement paper (`docs/plans/saga-paper.typ`). Released in response to external
review: the protocol is the durable contribution — this kit lets anyone re-run the
method on their own private corpus, which is the paper's external-validity path (the
corpus itself is private by construction and is not included).

Paths are genericized: `$HOME` = the operator home, `$SCRATCH` = a session scratch
directory, `USER` in project-dir slugs = the operator username. Scripts are archival
records of the exact protocol run on 2026-07-16..17; adapt paths to reproduce.

## E2 — Ratification-derived graded gold (`e2/`)

Graded relevance gold built from the operator's own dialog-acts + external ship
ledgers, with sealed pre-registration. Results: `docs/plans/saga-e2-results.md`;
protocol audit: `docs/plans/saga-e2-audit-packet.md`.

| File | Role |
|---|---|
| `queries.json` | 20 why-queries (12 CSR corpus, 8 app corpus), pre-registered |
| `gold.json` | Sealed pre-commit origin memories per query (sealed commit hash inside) |
| `extraction_prompt.md` | Dialog-act extraction prompt (DIRECTS/ACCEPTS/REJECTS/REASKS), dual-vendor |
| `prep_pools.py` | Build candidate pools from frozen rank lists (union of arms, no re-search) |
| `prep_digests.py` | Per-conversation digests; falls back to CSR DB chunks when JSONL purged |
| `prep_ledger.sh` | Extract git/npm/release-train ship ledgers (repo paths = operator's) |
| `run_extract.sh` | Dual-vendor extraction driver (grok + sonnet, strict consensus) |
| `map_helper.py` | Metadata-only origin mapping helper (literal LIKE + git history + dates — NO embedding search) |
| `grade.py` | Grading: acts × ledger corroboration → grades 0-3; grade 3 requires sealed+mapped origin |
| `mapping.json` / `grades.json` / `pools.json` | Outputs: origin map (owner-audited), per-item grades (204 items), pools. Conversation IDs only — no transcript content |

Rules that made it credible: extraction never sees rank lists; ledgers corroborate but
never mint the top grade; UNRESOLVED strata excluded from origin metrics; owner audit
of every disputed mapping.

## E1 — Seven-arm ablation (`e1/`)

One shared index build, seven retrieval arms (kNN → full walk, each component toggled).
Results: `docs/plans/saga-e1-results.md`. Harness: `examples/saga_ablation.rs` (in-repo).

| File | Role |
|---|---|
| `spec.md` | Five-part implementation spec for the harness (arms, env contract, output schema) |
| `grok_prompt.md` | Full lane prompt that produced the harness (archival) |
| `score.py` | TSV/JSONL → per-arm MRR/Recall/nDCG against E2 gold |

## E3 — Controlled contamination (`e3/`)

Dose-response injection: C0 frozen baseline, C1 +eval-design transcript, Csham
+size-matched unrelated transcript, C5 +5 scripted re-ask cycles. Exact brute-force
retrieval. Results: `docs/plans/saga-e3-results.md`. Harness: `examples/saga_contamination.rs`.

| File | Role |
|---|---|
| `spec.md` | Condition-construction spec (incl. sham selection rule: 70-130% size, zero query hits) |
| `gen_c5.py` | Synthetic re-ask cycle generator (marked synthetic; controlled injection, not natural ecology) |
| `queries.txt` | 8 eligible queries (origins pre-date C0) |
| `score.py` | Origin-rank / echo@10 / displacement / repair-delta scorer |

## Not included (privacy)

Conversation digests, extraction raw outputs, pooled transcript text, the commit-subject
ledger (spans private repos), and all corpus databases. Everything included is
metadata-level: IDs, grades, booleans, hashes, protocol.
