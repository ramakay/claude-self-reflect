# T3 Eval — Dropped / Re-typed Candidates (41 of 68)

27 candidates were selected into `final_set.md`. This file accounts for the other 41,
one line each. "Budget cut" means the candidate passed all four filters (leakage, key
verification, multi-hop audit, mechanical dedupe) but lost out in the final balance pass
(§5) to a stronger or less-redundant alternative — it is not a quality failure.

## CSR / candidates.md (27 dropped, of 38)

- **#2** — budget cut: redundant with kept #9/#1 (same resolution-ledger arc already covered).
- **#3** — mech-redundant: full answer (TaskCreate/TaskUpdate switch, 52/96/0 ratio) stated in single commit `d148b3d`'s own body; duplicates what the mechanical commit-pairs set already measures.
- **#4** — budget cut: task-lineage adds only corroboration; core answer is still single-commit-sufficient.
- **#5** — mech-redundant: full answer (60GB bloat, numbered-files cause) stated in single commit `0fb7a6e`'s own body.
- **#6** — mech-redundant: full answer (two-step consent split) stated in single commit `83f56b3`'s own body.
- **#8** — mech-redundant: full answer (supersession-boost rationale) stated in single commit `79a1765`'s own module doc.
- **#12** — budget cut: redundant abandonment-theme with kept #10/#11 (Stage D never ran) and #17 (paper walked back).
- **#13** — budget cut: redundant "field/signal stays inert" trap with kept #14 (supersedes field); #14 is the more airtight (multi-call-site-grep-verified) version.
- **#15** — budget cut: redundant "research finding not applied to production" pattern, already covered thematically.
- **#18** — budget cut: solid test-count-drift verification question, lower narrative priority than kept picks.
- **#19** — budget cut: same "not fully closed, residual window" epistemic-honesty pattern as kept #28 (orig); #28 kept as the sharper version.
- **#21** — budget cut: redundant with kept #20 (same branch-freshness theme, #20 is the more specific/citable one).
- **#22** — budget cut: paired dependency-pin-freshness question with #23; neither made the final cut for space.
- **#23** — budget cut: paired with #22 (same rmcp/ast-grep deferral pattern); dropped as a pair.
- **#24** — budget cut: valid timing nuance, lower distinctiveness vs. kept ratification-arc questions (#7/#10/#17 in final numbering).
- **#25** — budget cut: thematically adjacent to kept #20's freshness-testing; not distinct enough to also include.
- **#26** — budget cut: valid non-reversion verification, lower priority than kept abandonment questions.
- **#27** — MULTI-HOP AUDIT DOWNGRADE + drop: "was the bug pattern checked elsewhere" is answerable via git+codebase grep alone (no conversation-layer evidence actually required despite the original claim); re-typed C/single-hop, then not selected for final budget.
- **#29** — MULTI-HOP AUDIT DOWNGRADE + drop: no source at all (git/plan/task) records agent-concurrency counts, so there is no second "hop" to combine, just an absence; re-typed C/single-hop (pure epistemic-honesty test). Not selected — kept #28 (final #10) instead as the sharper version of this same lesson, since it at least has a verifiable sequencing half.
- **#31** — budget cut: strong task-lineage codegraph question, but that theme isn't otherwise represented in the trimmed final CSR set.
- **#32** — budget cut: strong task-lineage question, space constraint.
- **#33** — budget cut: strong task-lineage question, space constraint.
- **#34** — budget cut: strong task-lineage question, space constraint.
- **#35** — budget cut: valid "kept, not deleted" verification but overlaps thematically with kept #17/#10 ("did it survive/get walked back").
- **#36** — budget cut: interesting git-hygiene puzzle (duplicate commit messages), tangential to the kept narrative arcs.
- **#37** — budget cut: solid plan-grounded design-rationale question, but the two plans it draws on are already represented via kept #1/#7/#9.
- **#38** — budget cut: good multi-commit-sequence synthesis (not mech-redundant — spans 4 commits), but release-CI theme is already represented via kept #28 (orig)/final #10.

## private-app / candidates_private-app.md (14 dropped, of 30)

- **#40** — budget cut: [private repository candidate; details withheld]
- **#41** — budget cut: [private repository candidate; details withheld]
- **#43** — mech-redundant: [private repository candidate; details withheld]
- **#44** — budget cut: [private repository candidate; details withheld]
- **#45** — mech-redundant AND leakage-adjacent: [private repository candidate; details withheld]
- **#48** — budget cut: [private repository candidate; details withheld]
- **#50** — budget cut: [private repository candidate; details withheld]
- **#51** — REDUNDANT (duplicate evidence): [private repository candidate; details withheld]
- **#52** — budget cut: [private repository candidate; details withheld]
- **#53** — budget cut: [private repository candidate; details withheld]
- **#58** — budget cut: [private repository candidate; details withheld]
- **#65** — REDUNDANT (duplicate evidence): [private repository candidate; details withheld]
- **#66** — REDUNDANT (duplicate evidence): [private repository candidate; details withheld]
- **#68** — budget cut / too meta: [private repository candidate; details withheld]

## Drop-reason tally

| Reason | Count |
|---|---|
| Mechanical-set redundant (criterion 4) | 6 (#3, #5, #6, #8, #43, #45) |
| Multi-hop audit downgrade → dropped (criterion 3) | 2 (#27, #29) |
| Duplicate evidence (same ground truth key as a kept item) | 3 (#51, #65, #66) |
| Budget cut / thematic redundancy (balance pass, criterion 5) | 30 |
| **Total dropped** | **41** |

No candidates were dropped for LEAKAGE (criterion 1) or vague KEY VERIFICATION (criterion 2) —
the only leakage issue found (orig #30, a directly-quoted task ID) was fixed by rewording rather
than dropped; it is kept as final #11.

File: /tmp/scratch/t3-eval/drops.md
