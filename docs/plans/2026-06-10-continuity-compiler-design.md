# CSR Continuity Compiler — Cornerstone Design v2 (Post-Review)

> **Vision:** Kill a session, restart — Claude knows. An infinite session without infinite tokens.
>
> **Status:** v2 — reshaped after adversarial review by Codex (gpt, xhigh effort) and
> Gemini 3.1 Pro (web-grounded). Final rulings by Claude (Fable). v1 verdicts: both RESHAPE.
> **Author:** Claude (Fable) + Rama, 2026-06-10

---

## Objective (revised): Epistemic Continuity

v1 claimed "behavioral continuity" (don't re-ask, don't re-explore). Review exposed this as
a dangerous proxy: when the world changed between sessions, the *correct* behavior is to
break continuity and re-validate. The revised objective, per Gemini's reframe:

**Epistemic continuity** — the next session retains the *bounds* of prior knowledge
(what was established, what was assumed, what failed) and explicitly re-validates
assumptions against current state before acting on them. The agent doesn't pay twice for
what's still true, and doesn't trust what may have changed.

Novelty framing corrected (Codex): MemGPT/Letta, mem0, Zep/Graphiti, A-MEM, claude-mem,
and Claude Code CMV all occupy adjacent territory; CMV in particular shares the
"expensive understanding built once, reused" thesis. CSR's own injection predictor already
ranks by semantic + recency + file-overlap + error signals. Differentiation is the
*combination* below, not any single mechanism.

## Pillar 1 — Derivation Ledger (reshaped: forward-looking, coarse, auditable)

Memories priced by **estimated future re-derivation cost**, not historical sunk cost.
Review killed v1's economics on two grounds:

- **Sunk-cost fallacy (Gemini):** a fact that cost 18K tokens to discover may cost ~0 to
  re-derive if the user's next prompt states it, or if it's now one `git log` away.
  Pricing by history makes expensive facts unevictable parasites.
- **Attribution precision (Codex):** per-message `usage` charges whole-context, not
  marginal cost; one turn yields many facts; one fact emerges across failed branches.
  Precise fact-level token attribution from JSONL is not computable.

v2 ledger entry:

```
fact → (content, anchor, cost_bucket, inferability, confidence,
        times_reused, scope)

cost_bucket   ∈ {cheap, moderate, expensive}   — coarse, auditable brackets from
                turn count, tool-result volume, retry count. No false precision.
inferability  — discount: can this be re-derived from working tree or likely prompt
                in one cheap step? (file still exists, fact is in a comment, etc.)
scope         — repo + branch/worktree + user. Facts never cross scope silently.
anchor        — type-aware integrity probe (see Pillar 5): AST node for code facts,
                normalized-snippet hash for prose/config, none for decisions
                (permanently labeled `assumption`), age-only for `volatile` env facts.
```

Injection knapsack: maximize `P(needed) × bucket_weight × (1 − inferability)` per token
under budget. Honest claim: coarse economic ranking layered on the existing multi-signal
predictor — a differentiator, not a revolution.

## Pillar 2 — Checkpoint Compilation (reshaped: opportunistic cache + birth validation)

v1 claimed all compute at death, zero at birth. Review corrections adopted:

- Stop fires every response — it is not "death," and the user is present. Heavy compilation
  belongs in SessionEnd/PreCompact and the existing enrichment daemon; Stop does only the
  cheap incremental checkpoint it does today.
- Death-time images are an **opportunistic cache**, never the sole mechanism. Atomic
  writes (tmp + rename, already CSR convention), TTL, and a birth-time fallback path.
- "Zero compute at birth" is dead. Birth runs a mandatory cheap **validation pass**
  (<50ms, zero tokens): anchor-integrity checks on the checkpoint's facts (Pillar 5)
  plus git as a coarse volatility signal only (branch switch → bulk-demote
  branch-scoped facts). Contradicted facts are marked or dropped before injection.
  This answers Gemini's fatal objection (world mutates between Friday kill and Monday
  restart) without giving up precompiled speed. Git is NOT the verification oracle —
  file-level diffs are the wrong granularity and most facts are git-blind; anchors are.
- **Groundhog Day defense:** sessions that died in a failure state compile to a
  "what didn't work" frame (failed_approaches prominent, no "continue the plan"
  directive) — never a resume image that replays the dead end.
- Concurrency: per-project compile lock (fs2, existing pattern); last-writer-wins with
  session-id provenance.

Latency promise kept: birth cost = file read + git checks + local embed (<50ms warm),
no LLM call, no agent, no user-visible delay.

## Pillar 3 — Crystallized Memory (reshaped: advisory-first, visible, scoped)

Review verdict was the harshest here (Codex: KILL auto-blocking; Gemini: zero-token
invisibility causes self-gaslighting). v2:

- **Advisory by default.** Auto-generated guards WARN with provenance ("CSR: corrected
  3× on 5/12, 5/14, 6/02 — project uses pnpm; override: --csr-allow"), never silently
  block or rewrite. The model and user both see why.
- **No generated code.** Guards are data-driven policy entries (pattern + message + scope
  + TTL) evaluated by the csr-engine binary — transcript text never becomes executable
  shell. Eliminates the injection class.
- **Hard blocks are manual + verifiable only.** Promotion to blocking requires explicit
  user action AND a repo-verifiable invariant (e.g. pnpm-lock.yaml present). N≥3
  recurrence, branch/worktree scope, TTL, per-session bypass, dry-run mode, global kill
  switch, allow/deny counters — all mandatory.

What survives of the original idea: memory that *acts at the tool layer* with zero
standing tokens until triggered — but it speaks when it acts.

## Pillar 4 — Closed-loop Governor (kept, with honest measurement)

Track downstream reuse of injected facts (file overlap, reference detection); shrink
budget where injections go unused; go silent below threshold. Review caveats adopted:

- "Tokens saved" is counterfactual — claims require a holdout: suppress injection for a
  random ~10% of sessions and compare exploration spend. Without holdout, report reuse
  rates only, never savings.
- Anti-flap: minimum sample sizes before muting; decay rather than hard cutoff, so one
  coincidental miss doesn't silence a useful project.
- Side effect stands: reuse × cost_bucket = the enterprise ROI instrument, logged.

## Pillar 5 — AST-Anchored Memory (the moat)

CSR never stamps a fact `true` — it stamps **anchor integrity**: provenance, age, reuse
count, and whether the thing the fact points at still exists in the form it had when the
fact was learned. The model gets calibrated assumptions, not false certainty.

### Why not git, why not byte hashes

Git diffs are the wrong granularity: a formatting commit "invalidates" still-true facts;
an edit in a *different* file silently invalidates a true one; rebases/squashes/branch
hops churn SHAs without semantic change. Naive span hashes fail the same way — rustfmt or
prettier drifts every hash, and line-number anchors break when code above moves.

### Anchor mechanism

Code facts anchor to **structural nodes, not text locations** — ast-grep is already in
the binary (6 languages: Rust, Python, TS, JS, Go, TSX):

```
anchor = (file, node_kind, qualified_name, normalized_body_hash)
         e.g. (middleware.rs, function_item, validate_token, h1)
```

Verification re-parses the file (ms-level) and returns a **graded verdict**, never a
boolean:

```
node found, normalized body matches   → intact
node found, body changed              → modified — re-verify before relying
node renamed/moved (fuzzy match)      → relocated — re-anchor automatically
node gone                             → broken — drop or demote
```

Normalization (strip whitespace/comments before hashing) makes formatting, reordering,
and relocation within a file invisible. Only semantic change to the anchored code fires —
exactly the signal wanted. Non-code anchors (markdown, TOML, YAML) fall back to
normalized-snippet content search: anchor by content, not line numbers.

### Birth-time: the continue-vs-new decision becomes a symbol join

Risk at session start: user begins a new feature (last session's context is noise).
Hedge: user continues (context is gold). AST anchors convert this bet into a measurement
that runs before any token is spent:

```
DEATH:  ast-grep → checkpoint anchors: {validate_token: h1, refresh_session: h2, ...}

BIRTH:  first prompt arrives → symbol join (0 tokens, <10ms)
          │
          ├─ prompt overlaps anchored symbols ("token validation bug")
          │     → CONTINUE: inject function-level state            (~300 tok)
          │
          ├─ anchors CHANGED since checkpoint (edited between sessions)
          │     → inject the DELTA: "validate_token modified since
          │       your checkpoint — your memory of it is stale"    (~150 tok)
          │
          └─ no overlap, new domain ("add billing page")
                → PARK: one line — "auth workstream parked,
                  handle ep_7f3a" — anchors wait                   (~30 tok)
```

The asymmetry makes the hedge nearly free: wrong-inject ≈ 300 wasted tokens; wrong-skip ≈
10–50K of re-exploration; the join means we rarely pay either (new-feature branch costs
~30 tokens of parked handle).

### Function-level resume granularity (~10x over file-level)

"You modified middleware.rs" forces a whole-file re-read (~15K tokens for 2K lines).
"You modified `validate_token` and `refresh_session`, added `TokenError`, bodies unchanged
since, tests not yet run against them" → model reads two functions (~1K tokens) and
continues. The granularity IS the token saving.

### Use-time firing: parked anchors pay deferred

New features touch old code eventually. PreToolUse: model is about to edit an anchored
function → its history surfaces just-in-time ("changed last session for X, test coverage
Y"). The "new feature" branch converts injection from wasted-at-start to
delivered-at-use. Verification is lazy by the same mechanism — anchors re-check at the
moment of reliance, so cost scales with use, not inventory.

### Epistemic hot-spot coverage

Last session's modified functions are precisely where the model's stale confidence is
most dangerous — it "remembers" writing them. AST delta at birth flags semantic change in
exactly that hot set: the epistemic-continuity contract enforced where it matters most.

### Honest limits

Anchors cover code facts in the 6 parsed languages. Decisions, preferences, and
environment facts remain label-only (`assumption` / `volatile`, aged). The
in-conversation oracle backstops everything: facts contradicted mid-session get demoted
by the governor — memory that loses arguments loses budget.

### Competitive position

Summary-based tools (claude-mem, CMV, mem0-style) store text; their best routing is fuzzy
prompt-vs-summary matching at file granularity, and they have nothing addressable for
use-time firing. AST-anchored memory — facts pinned to code structure, surviving
formatting and relocation, with symbol-level injection routing — requires a structural
parser in the data path. CSR already ships one; competitors rearchitect to copy.

## Adversarial-systems requirements (new section — from review Q6)

Memory extraction, fact scoping, and guard generation are adversarial surfaces, not
convenience features:

1. **Sanitization:** facts and guard messages pass the existing injection-pattern
   sanitizer; secrets (API keys, tokens) redacted before any fact is stored — a derived
   fact must not outlive a redacted transcript.
2. **Poisoning model:** README text, test output, or hostile content in transcripts can
   masquerade as "user corrections." Correction extraction requires user-authored
   messages only, never tool_result or file content.
3. **Scoping contract:** every fact and guard carries {repo, branch/worktree, user}.
   Nothing auto-commits to the repo; ledger lives in ~/.claude-self-reflect/ (local,
   per-user) — prevents pathogen spread via git.
4. **Schema drift:** JSONL parsing failures degrade to "no fact extracted," never to
   low-confidence guesses; parse-error rate is a telemetry alarm.
5. **Storage failure:** corrupt index/db → silent fresh-start with stderr log, never a
   blocked session (existing catch-all hook convention).
6. **Evaluation harness:** benchmark suite of resumed-task scenarios + bad-memory corpus
   + forced-failure hook tests, before any "it works" claim.

## Patent posture (revised: build first, file narrow later, maybe)

Codex's Alice/Mayo (§101) and obviousness (§103) analysis accepted: broad claims on
"rank memories by value, precompile context" are abstract information processing over
generic components, and obvious over MemGPT + mem0 + Zep + CMV. Gemini independently
rated Pillars 2–4 weak (prefetching / AHE / bandit prior art) and Pillar 1 strongest.

Decision: **no filing now.** Build Pillars 1+4, develop the concrete attribution-bucket +
reuse-feedback algorithm, run the holdout eval. If the implemented mechanism is genuinely
distinct, file a narrow defensive provisional on: *"dynamically weighting LLM context
injection by empirically measured re-derivation cost with closed-loop reuse feedback"*
(1+4, per Gemini) — framed as a specific technical improvement, with a prior-art chart.
Competitive moat meanwhile = execution + the local/verified angle, not claims.

## Build sequence (revised)

- **A (finish v9.2):** episode persistence + briefing. Unchanged, in flight.
- **B:** TodoWrite/ExitPlanMode extraction, episode chains, Tier-0 identity line with
  retrieval handle, birth-time validation pass. (Unaffected by review objections.)
- **C:** AST anchor capture at death (signatures of modified functions into checkpoint);
  birth-time symbol join (continue/delta/park routing); graded anchor verification.
- **D:** Derivation Ledger with cost buckets + inferability; knapsack injection layered
  on existing predictor; governor with reuse tracking. PreToolUse use-time anchor firing.
- **E:** Advisory crystallized warnings (data-driven, scoped, TTL). Holdout eval harness.
  Hard-block promotion ships only after advisory telemetry proves precision.

---

## Review record

- **Codex (xhigh):** overall RESHAPE. Q1 reshape (CMV prior art; CSR's own predictor
  contradicts novelty claim), Q2 reshape (coarse buckets only), Q3 reshape (Stop≠death;
  opportunistic cache), Q4 KILL auto-blocking guards, Q5 KILL broad patent (Alice/§103),
  Q6 reshape (schema drift, poisoning, scoping, secrets, eval counterfactual).
- **Gemini 3.1 Pro:** overall RESHAPE. Ledger reshape (sunk-cost flaw), pre-materialization
  KILL (mutated world; Groundhog Day), guards reshape (visibility; TTL), objective reshape
  (epistemic continuity), patent: file 1+4 narrow, not 1+2.
- **Final call (Fable):** adopted epistemic continuity; forward-looking coarse-bucket
  ledger; death-time compile retained as cache with mandatory birth validation (rejecting
  Gemini's full kill — birth-only compute reintroduces the latency the design exists to
  remove); guards advisory-first per Codex; no patent filing until post-implementation.
- **Post-review refinement (2026-06-10):** git-verify replaced by AST-anchored memory
  (Pillar 5) after "git is fickle" critique — git diffs are wrong granularity, byte
  hashes drift under formatters. Anchors are structural (ast-grep nodes, normalized-body
  hashes), verdicts graded not boolean, verification lazy at use-time, and the
  birth-time symbol join resolves the continue-vs-new-feature routing risk.
