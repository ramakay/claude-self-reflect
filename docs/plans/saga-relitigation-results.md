# Re-litigation Benchmark — Results (run 2026-07-20, graded 2026-07-22..24)

Behavioral test of Naur-style theory restoration: does conversation memory stop an
agent from re-litigating settled decisions — and does reinstatement recall earn
separation from plain similarity search while doing it? Designed under binding
cross-vendor advisor conditions (Grok 4.5): similarity-only kNN control arm
required, ≥7 headline tasks from post-tuning decisions, blinded human rubric
grading, four-way task-class mix, arms identical except retrieval path.

## Design

- **16 sealed tasks** (12 headline + 4 calibration), sha256-sealed before any arm
  ran (`eval-kit/relitigation/tasks.sealed.json`, seal `749fc0f8…`). Four classes,
  pre-registered correct action per task: settled-for-cause (decline+cite, 4),
  should-change (accept, 4), irrelevant-past (ignore past, 2), ambiguous
  (check/ask, 2) + 4 calibration.
- **Three arms**, identical prompt/harness/model (`claude -p`, Sonnet), differing
  only in retrieved context: **R** = production reinstatement recall; **K** =
  similarity-only kNN (E1-style, chunks + reflections, max-score merge); **N** =
  no memory. Context rendered by one shared function — no scores, no arm-revealing
  wording; deterministic ordering; hooks and tools disabled in test dialogues.
- **Leak defense**: arms ran against a frozen DB snapshot scrubbed of the design
  session and its subagent conversations + derived reflections; retrieval outputs
  verified to contain zero scrubbed ids.
- **Grading**: operator graded all 36 headline responses (12 tasks × 3 arms)
  blinded — arm identity hidden behind shuffled X/Y/Z codes, key sealed until all
  grades were in. Rubric 0–3 reason-faithful action: 3 = correct action + true
  reason; 2 = correct action, partial reason; 1 = wrong action but serious
  engagement (or correct action, wrong reason); 0 = careless.

## Results

### Action level (all 16 tasks, pre-registered correct actions)

Memory arms 12/16 correct actions each; no-memory 8/16. Reinstatement and kNN
produced **identical verdicts on all 16 tasks**.

Sharpest single datapoint: on the calibration task replaying the integrity-check
performance fix, the no-memory arm ACCEPTS re-breaking a shipped 11.4s→11ms fix;
both memory arms decline citing the original incident.

### Reason quality (12 headline tasks, blinded human rubric, max 36)

| Class | Reinstatement | kNN | No-memory |
|---|---|---|---|
| settled-for-cause (4) | **10/12** | **9/12** | 6/12 |
| should-change (4) | 6/12 | 6/12 | 6/12 |
| irrelevant-past (2) | 6/6 | 6/6 | 6/6 |
| ambiguous (2) | 5/6 | 6/6 | 6/6 |
| **Total** | **27/36** | **27/36** | **24/36** |

## Findings

1. **Memory prevents re-litigation exactly where theory predicts.** On
   settled-for-cause tasks both memory arms nearly double the no-memory arm's
   reason quality (10 and 9 vs 6) and the no-memory arm's failures are the
   dangerous kind — accepting requests that re-open shipped fixes. This is the
   behavioral cash-value of Naur theory restoration: the reason a decision was
   settled survives only through memory.

2. **Reinstatement gained no separation from plain kNN — at either level.**
   Identical actions on 16/16 tasks; tied 27–27 on blinded reason quality. On
   single-hop re-litigation tasks ("does history bear on this request?"), one-shot
   similarity retrieval restores the theory as well as the full reinstatement
   walk. The reinstatement spike's +53% advantage lived on multi-hop provenance
   queries; this benchmark's tasks are single-hop, and the tie localizes the
   invention's value to the multi-hop regime — the next benchmark, if run, should
   be built of tasks whose justification chain spans multiple conversations.

3. **Uniform conservatism bias, arm-independent.** All three arms scored 6/12 on
   should-change: valid changes (a forcing-feature dependency bump, a
   now-satisfied precondition, an ablation-backed rebalance) were met with
   NEEDS-INFO instead of acceptance in 10 of 12 responses (every arm accepted only
   the intent-margin task, where the request itself carried a week of telemetry).
   Memory did not cause the caution (no-memory shows it identically) — but memory
   also did not cure it: retrieving the pin's own exit condition did not stop the
   arm from asking for permission it already had. Practical consequence: a
   memory-augmented agent's failure mode shifts from "re-breaks settled things"
   toward "over-defers on justified change," and task mixes that omit
   should-change classes would miss this entirely.

4. **No over-generalization of the negative result.** On irrelevant-past tasks
   (including one proposing edge-level ratification right after Gate A′ killed
   node-level) every arm correctly declined to treat inapplicable history as
   binding — perfect 6/6 across all arms. The feared failure mode "memory of a
   failure blocks structurally different follow-ups" did not appear.

## Verdict

Memory's benefit on re-litigation is real and class-specific (settled-for-cause);
reinstatement recall is not the source of that benefit on single-hop tasks — plain
similarity retrieval suffices. The invention's claim now rests where the spike
found it: multi-hop provenance. Honest summary for the paper: *a blinded,
pre-registered, three-arm behavioral benchmark shows conversation memory roughly
halves reason-quality failures on settled decisions (10+9 vs 6 of 12), while
showing no reinstatement-over-kNN separation in the single-hop regime.*
