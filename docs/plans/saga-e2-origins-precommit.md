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

## CUED stratum (post-seal second pass, 2026-07-17 — same sitting, cues disclosed inline)

Cue source: session memory files + repo docs + git metadata. NO CSR retrieval used. These
entries aid mapping/audit and may support a separately-reported cued-recall metric; they
NEVER enter the sealed origin-MRR gold.

| Q | Cue given | Owner answer (verbatim option / text) |
|---|---|---|
| Q1 | v8 Rust rewrite, rusqlite Connection not shareable | "Agent build decision" — emerged during build, accepted not directed |
| Q3 | v9.2 slow-status incident, 11.4s→11ms meta-table cache | "I flagged slowness" — owner-directed |
| Q7 | triage week ~2026-07-10, PR #230 | "I reported bad results" — owner-directed |
| Q11 | Route A continue/resume, keywords vs exemplars, research-verified | "I directed research" — owner-directed |
| A1 | Core-3 finalize vs legacy setActive, sign-in bug vs upgrade | "ui was not showing signed in user after clerk signed in, hence expo" |
| A2 | defer prompt, record intended action, prompt later | "Conversion data driven" — funnel/drop-off numbers |
| A4 | PostHog anonymous→identified merge mismatch | "I questioned numbers" — owner noticed dashboard mismatch |
| Q2 (audit) | rerank.rs impl 2026-07-07 vs owner's pre-v8 claim | "Idea old, impl July" — concept pre-v8, implementation July; sealed OUT-OF-CORPUS entry stands, mapping should look for BOTH an early concept conv (may be out-of-corpus) and the July implementation conv (grade 2 candidate) |

Cued tally: 7/7 previously-unresolved now have cued recollections (4 owner-directed, 1
agent-decided, 2 event descriptions); Q2 discrepancy resolved as idea-old/impl-July.
Agent-decided note (Q1): "origin" may legitimately be a deliberation trace, not an intent
trace — the originating conversation is where the agent made + owner accepted the build
decision. Extraction should not assume DIRECTS exists for every query; absence of DIRECTS
with ACCEPTS+edits = grade 2 ceiling per protocol, and that is the CORRECT label for
agent-originated decisions. This is itself a finding: some provenance is agent-originated.
