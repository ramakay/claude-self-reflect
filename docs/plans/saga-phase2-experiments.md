# Saga Phase 2 — Experiment Program (paper revision 2)

Driven by external GPT-Pro review (2026-07-15) + grok-advisor design consult. Goal: convert
"impressive dogfooding report" into "defensible research paper" — isolate causality, harden GT,
characterize contamination.

## Ordering (advisor ruling: metric before mechanism before polish)

1. **E2 — Graded provenance gold** (file-touch GT is the paper's soft underbelly)
2. **E1 — Factored ablation grid** (mechanism isolation meaningful only once metric believable)
3. **E3 — Controlled contamination** (already documented qualitatively; controlled dose = polish)

## E2 — Graded provenance gold (FIRST) — REVISED 2026-07-17: ratification-derived gold

**Redesign ("the corpus labels itself"):** owner-graded pilot replaced by Option C hybrid
(grok-advisor ruled 2026-07-17; owner had no grading time — 4-6h → ~1h). Labels mined from
the corpus's own operator interaction traces at runtime: contemporaneous dialog-acts anchored
to artifacts, NOT retrospective annotation. Kills lived-history bias (labels predate eval,
on-screen only).

### Protocol (Option C — advisor verdict verbatim: "origin-MRR is the paper's load-bearing
claim and cannot rest on LLM-extracted gold")

1. **Pre-commit origins FIRST**: owner writes down the ~20 grade-3 origin conversations from
   memory BEFORE seeing any retrieval ranks or extraction output. These are the human-anchored
   grade-3 labels. Timestamped file, committed before any rank table exists.
2. **Pool**: file-touch history ∪ both arms' top-10s per query. **Pool-injection rule**
   (advisor's biggest-unlisted-risk fix): owner-listed origins missing from the pool are
   INJECTED and disclosed as injected — otherwise ratification only re-scores a proxy pool and
   true origins stay invisible. Report injection count. Freeze pool before extraction.
3. **Extraction (grades 0-2)**: LLM extraction, EXTRACTIVE quote-anchored questions only
   ("does the operator direct this work in this excerpt? quote the turn"), never holistic 0-3
   judgment. Dialog-acts per candidate conversation:
   - DIRECTS — imperative operator turn before edits touching target artifact
   - ACCEPTS/REJECTS — operator turn after edits/tests
   - DISCUSSES — mentions target, no edits, post-dates work
   - RE-ASKS — question-shaped, echoes eval query
   Cross-vendor dual extraction; report inter-extractor agreement.
4. **Grade map**: 3 = DIRECTS + ACCEPTS + edits target (grade 3 ONLY confirmable against the
   pre-committed origin list; extraction alone never mints a 3); 2 = edits target, direction
   originated elsewhere; 1 = DISCUSSES post-hoc; 0 = RE-ASK/echo.
5. **Implicit-ratification rule** (fixed before extraction; advisor: sound as weak-accept
   prior, with conflation hole patched): absence of rejection + subsequent work that
   GENUINELY BUILDS ON the artifact (test/extend/depend — not open-and-close) within a
   temporal window = weak accept. Never upgrades to grade 3 without a DIRECTS span. Report
   orphan rate (abandoned artifacts that never earn implicit accept) separately.
6. **Owner audit**: stratified ~50-item sample (~30 min) → human-model κ replaces self-κ.

### Positioning (advisor ruling 2)

Do NOT name this a methods contribution. One tight Methods paragraph: "GT construction
protocol for this pilot." Paper already carries novel-term load (self-indexing evaluation
contamination) + circularity surface; naming invites the Joachims pattern-match AND "GT
encodes the thesis" as a second front. Contribution = retrieval result under disclosed
protocol, not the protocol brand. ("The corpus labels itself" stays as internal shorthand
and possible future short paper.)

### Disclosures (mandatory)

- GT derived from same corpus being searched — unavoidable for in-situ provenance GT
  (file-touch had it too). Defense: feature disjointness (extraction sees dialog-acts + edit
  adjacency; retrieval sees only embedding similarity — kNN can find a DIRECTS+ACCEPTS conv
  equally well) + construct validity (grade 3 IS the definition of origin, not a proxy).
- LLM extraction step — mitigated by extractive quote-anchored design + audit κ.
- Pool incompleteness survives grading even with injection — graded labels ≠ complete gold.

### Prior art (novelty sweep 2026-07-17, arXiv IDs API-verified)

- **CoRet (arXiv:2505.24715, ACL 2025) — closest threat**: PR patches + call graphs as
  auto-generated retrieval gold for code editing. Binary relevance, single-turn PR text, no
  conversational ratification, no grading. One extension away (multi-turn transcripts +
  dialog-acts). Cite and differentiate.
- As It Was (arXiv:2607.01040, SIGIR 2026): behavior-grounded LLM judge from clicks/dwell —
  (query, doc) relevance, not (artifact, origin-conversation) provenance.
- UNO (arXiv:2602.06470): preference pairs from user logs for LLM optimization — training
  signal, not eval gold.
- Joachims 2002 lineage owned explicitly in Related Work: implicit feedback labels search
  behavior; this labels ratification behavior, query-independent.
- (X-SYNTH claim from sweep had unverifiable ID — excluded.)

### Metrics

origin-MRR (first grade-3; human-anchored), nDCG@10 (grade weights), Recall@10 at grade ≥2.
File-touch demoted to candidate-set construction only.

### External ratification ledgers (2026-07-17 — off-corpus acceptance signals)

Acceptance signals are NOT only in the JSONL. Independently timestamped systems outside the
corpus provide ratification events — GT anchored there is immune to the circularity attack
(label not derived from the corpus being searched). Correlation mechanic: conversation →
external event by TIMESTAMP WINDOW + file/artifact overlap, not trailer parsing (git-trailer
bridge failed before: sparse 60/1096 + self-contaminating ID-matching; time-window correlation
works on all commits, and extraction runs against the frozen corpus so querying git cannot
contaminate the eval).

Acceptance LADDER (replaces binary ACCEPTS; implicit-ratification rule mostly dissolves into
hard external events):

| Strength | Event | Source | Scope |
|---|---|---|---|
| 1 weak | commit (local, reversible) | .git | all projects |
| 2 | git push origin | .git reflog/remote | all projects |
| 3 strong | PR merge to main (review approvals) | gh API | all projects |
| 4 | tag / release | .git tags | all projects |
| 4 | **npm publish** (registry timestamps, public) | npm registry API | **claude-self-reflect ONLY** |
| 4 | eas submit / fastlane deliver (App Store review = external human acceptance) | release-train.yaml + EAS | anukriti app |
| 4 | eas update OTA (release-train.yaml = purpose-built ratification ledger) | release-train.yaml | anukriti app |
| 4 | vercel production promote (+ rollback history) | vercel API | command center |
| 4 | GH Pages deploy | Actions | docs-site |
| 3 machine | GitHub Actions green on main; pre-commit suite pass | Actions / transcript | all projects |
| 5 terminal | product usage (PostHog events on shipped feature), campaign launched (Meta API), track live in app, MRR | product APIs | anukriti family |

NEGATIVE ledger (rejection labels — rare in any GT): git revert / vercel rollback / OTA
rollback / campaign paused shortly after a conversation's ship event demotes that
conversation's outcome.

Grade-3 upgrade: DIRECTS in-dialog + ship event off-corpus within window = origin corroborated
by external ledger — neither model-labeled nor memory-only; strengthens the pre-committed
origin list against the advisor's "model-labeled origins" collapse scenario.

### Final advisor sweep (2026-07-17) — cleared; locked rules

- Ledger = corroboration of ACCEPTS only, NEVER a second way to mint origin. Ship-proximal
  session without DIRECTS stays ≤2 regardless of ladder rung.
- Interview: first pass PURE MEMORY (date, artifact, who directed) — sealed + timestamped
  before any ledger content shown. Cueing fine after seal, never during elicitation. Freeze
  inclusion criteria before interview ends (no post-hoc drops once ranks exist).
- Description→conv-ID mapping: metadata-only (date window, project path, filenames, literal
  grep) — NO embedding/CSR search (back-door circularity). Unresolved maps dropped from
  origin-MRR, never soft-matched.
- Ledger correlation (BLOCKING before ledger grades): artifact-path overlap AND
  nearest-prior-DIRECTS-bearing-session only; 72h commit→session window (extend only for
  continuous same-branch/path work); squash attributes to first commit introducing path
  content; report multi-match rate, drop ambiguous links. Negative-ledger reverts need
  path-overlap+window or ignored. Ladder strata stay project-scoped.
- Out-of-corpus origins (pre-v8/Qdrant-era): capture in interview, separate stratum,
  EXCLUDED from origin-MRR (not dropped-as-fail, not capped at 2).
- Freeze dual-extractor prompts before pool freeze.
- Sequencing: pure-memory pre-commit → seal → metadata-only mapping → ledger correlation
  parallel after seal.

### E2b — cross-persona stratification (proposed extension)

Machine hosts 5 personas, 3 artifact ontologies: claude-self-reflect (Rust systems; ratify =
commit/test/merge), anukriti-meta-campaigns (performance marketing; ratify = campaign
launched/spend approved), anukriti iOS/Android (Expo eng; ratify = eas submit/OTA publish —
`release-train.yaml` is an external ratification LEDGER, hardest anchor available),
anukriti-campaigns (media production; ratify = track released), command center (dashboard;
ratify = deploy). Stratify query set ~4-6/project. Upgrades "two code corpora" to "one
operator, five personas, three artifact ontologies." Extractor design constraint: anchor on
file/tool events generally (PostToolUse tracking + publish commands), NOT AST-only — code
graph covers only code projects. Adds ~half day.

## E1 — Ablation grid (SECOND)

All arms share: provenance reranker, dedup, min_score, k=10 budget, ONE shared HNSW build per
grid run. Candidate generation varies:

| Arm | Generation |
|---|---|
| a | one-shot kNN |
| b | kNN + echo demotion |
| c | dense centroid PRF (Rocchio: one re-query from mean of top-3 seeds, 0.65/0.35) |
| d | blend-only per-seed walk |
| e | graph-only expansion |
| f | full walk |

- Advisor: NO RM3/BM25 lexical arm — claim lives in embedding space; dense Rocchio (c) is the
  right classic-PRF foil. Lexical bake-off = different paper. (Entity-Collision already shows
  RM3 null on intent queries — cite, don't rerun.)
- N=3 index rebuilds ONLY if arm deltas fall inside documented ±1 ANN noise band.
- Key comparisons: (c) vs (d) = per-seed vs collapsed-centroid (the PRF differentiation claim);
  (b) vs (f) = does the walk earn its cost over echo-defended kNN; (d)+(e) vs (f) = channel
  additivity.
- Review-2 advisor check (2026-07-16): arms (d)/(e)/(f) ARE the three-trace channel ablation
  reviewer's major 1 demands — grid answers it; not just defense/reranker variants. Arm (b) is
  verbatim the "kNN + echo demotion + same reranker" baseline review-2 predicts reviewers will
  request.
- Risk flagged: scope creep into multi-modality bake-off that never ships. Grid is 6 arms ×
  20 queries, one evening.

## E3 — Controlled contamination (THIRD)

- C0 = session-zero snapshot: `~/.claude-self-reflect/backups/pre-test-episode-sweep-20260611`
  (pre-dates all eval dialogue). **Drop/subset queries whose target decisions post-date C0.**
- C1 = C0 + only the eval-design transcript.
- Csham = C0 + unrelated transcript matched for length + tool activity (essential control).
- C5 = C0 + five scripted re-asking cycles — frame ONLY as "controlled self-referential
  injection" (dose-response), never as natural-ecology observation. Risk: proving "we wrote
  echo into the corpus" instead of reproducing real dynamics — mitigate by comparing C1
  (natural) vs C5 (dosed) trajectories.
- Retrieval: exact brute-force scan for this experiment (kills ANN variance; legitimate since
  IV is corpus content, not index algorithm; disclose production uses HNSW).
- Measures: origin-session rank, echo-count in top-10, displacement events, repair delta from
  chunk-level and conversation-level defenses.

## Out of scope for these three (reviewer's next ask, requires outside help)

- Multi-operator / held-out human labels; external corpora. None of E1–E3 fixes
  generalization or statistical power (n≈20, single operator, owner-labeled). Scope all claims
  within-operator until then. Bootstrap CIs meaningful only after E2 grades exist.

## Infrastructure notes

- Frozen probe harness proven: scratchpad probe.py pattern (MCP stdio JSON-RPC, --db-path
  clone, empty --projects-dir blocks import contamination). Judge packets + kappa.py reusable.
- Grid arms (c)/(d)/(e) need ReinstateConfig switches or a spike-style side binary — check
  whether config flags suffice before writing new code.
- conv_ prefix regex gotcha: \b fails after underscore — strip prefixes before UUID matching.
