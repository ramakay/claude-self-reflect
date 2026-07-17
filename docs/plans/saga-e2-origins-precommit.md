# E2 Pre-Committed Origin Labels — SEALED pure-memory pass

Sealed: 2026-07-17. Protocol: owner answered from pure memory only — no retrieval ranks, no
ledger cues, no candidate lists shown during elicitation (advisor rules, see
saga-phase2-experiments.md E2 "Final advisor sweep"). Answers recorded verbatim, typos
included. This list is frozen: cued follow-ups (post-seal, disclosed) form a separate
CUED stratum and never revise these entries.

Strata: MEMORY = pure-memory origin description; UNRESOLVED = dropped from origin-MRR;
OUT-OF-CORPUS = separate stratum, excluded from origin-MRR (not failed, not capped).

## Primary corpus (CSR, 12 queries — texts in csr-engine/examples/saga_spike.rs)

| Q | Query (short) | Stratum | Owner answer (verbatim) |
|---|---|---|---|
| Q1 | sqlite mutex | UNRESOLVED | Don't remember |
| Q2 | scaffold demotion | OUT-OF-CORPUS (owner-claimed pre-v8) | Pre-v8 / out-of-corpus |
| Q3 | integrity cache | UNRESOLVED | Don't remember |
| Q4 | narrative model chain | MEMORY | "operator noticed that we were not using latest haiku - usually llm providers provide monikers that resolve to latest versions so should use haiku vs haiku-04 etc." |
| Q5 | import skips CSR-agent convs | MEMORY | "CSR itself was querying its own fetches creating circular outcomes" |
| Q6 | tool results / chunking | MEMORY | "tool results were noisy if i recall correctly and having csr tool results was even more noisier" |
| Q7 | tiny hnsw exact scan | UNRESOLVED | Don't remember |
| Q8 | rmcp 1.6 pin | MEMORY (weak) | "some memory of a conflicting component O" |
| Q9 | hook catch-all wrappers | MEMORY | "latency between usersubmit and starting and ending claude was unacceptable." |
| Q10 | memory manifest header | MEMORY | "session start was through many itereations there was junk being provided that claude would frequently ignore as noise at start." |
| Q11 | intent semantic exemplars | UNRESOLVED | Don't remember |
| Q12 | fts5 fallback | MEMORY | "retreival wws poor and was not useful in the topic/subject being queried upon" |

## Second corpus (anukriti, 8 queries — texts in prior-session probe2.py, copied to spec)

| Q | Query (short) | Stratum | Owner answer (verbatim) |
|---|---|---|---|
| A1 | Clerk setActive | UNRESOLVED | Don't remember |
| A2 | auth intent deferral | UNRESOLVED (owner asked for context — deferred to CUED stratum) | "need more surrounding context to report." |
| A3 | snapshot cache | MEMORY | "there was frequent rate limiting by Meta" |
| A4 | posthog user counts | UNRESOLVED | Don't remember |
| A5 | score_save observability | MEMORY | "for anukriti command center to measure ho wmany users reach score state" |
| A6 | WhatsRunning | MEMORY | "what boosts/api retargets are running, later discussions also featured youtube and other promptions to get idea of budget as well as current boost state" |
| A7 | radio reel remotion | MEMORY (ambiguous referent) | "not sure which radio reel this is talking about but remotion for app store used for promo video and for japa and radio features iit would have been single scene." |
| A8 | lessons posthog | MEMORY | "UI actions sometimes are not in supabase" |

## Tally

MEMORY 11 (one weak, one ambiguous-referent) · UNRESOLVED 7 · OUT-OF-CORPUS 1 (owner-claimed).

## Audit notes recorded at seal time (post-seal observations, do NOT revise entries)

- Q2: owner claims pre-v8; session memory notes scaffold demotion landed in rerank.rs
  2026-07-07 (v9.x era). Discrepancy goes to mapping-audit — possible the *idea* originated
  pre-v8 while implementation is recent, or owner misremembers. Resolve via metadata-only
  mapping; sealed entry stands.
- A7: query's referent ambiguous to owner — flag for query-wording review in E2; candidate
  for drop-with-disclosure if referent cannot be fixed without retrieval.
- Next: CUED second pass (disclosed) for Q1, Q3, Q7, Q11, A1, A2, A4 — cue source = session
  memory files + repo docs + git metadata, never CSR retrieval. Cued answers aid
  mapping/audit only; origin-MRR gold remains this sealed list.
