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

## Anukriti / candidates_anukriti.md (14 dropped, of 30)

- **#40** — budget cut: redundant with kept #49; #49 tests the actual outcome/invariant (a richer question) on the same OTA-group-id feature.
- **#41** — budget cut: redundant with kept #67 (same skill-directory subject); #67's finding (it's secretly its own git repo) is the sharper, more surprising question. Also, #41 has no git artifact to check at all.
- **#43** — mech-redundant: full answer (ticket-vs-receipt distinction, 696-vs-305 numbers) stated in single commit `20e2cec`'s own body.
- **#44** — budget cut: third question on the same OpenAI-Ads-pixel plan; kept #55/#63 already test the harder "shipped or not / which repo" angles on this material.
- **#45** — mech-redundant AND leakage-adjacent: the commit *subject line itself* ("Saadhana tab replaces Japa tab behind kill-switch") already states the full answer verbatim — trivially retrievable, and risks leaking the answer into any grep-based search.
- **#48** — budget cut: fourth question on the OpenAI-Ads plan; redundant with kept #55/#63.
- **#50** — budget cut: valid B-type but least distinctive among the B-bucket candidates; kept #47/#49 instead.
- **#51** — REDUNDANT (duplicate evidence): ground truth key is explicitly the same dataset as kept #61 ("See #51/#61's evidence" in the source file). Kept #61 as the sharper, more surprising framing (cross-repo split) over #51's simpler "did it ship" framing.
- **#52** — budget cut: redundant with kept #55's "commit assertion alone doesn't prove X" epistemic-limit lesson; #55 tests the same limit with a more concrete (working-tree-file) payoff.
- **#53** — budget cut: redundant with kept #64 (identical task, identical "not implemented" finding); #64 is the more rigorous version (broadened multi-repo, under-any-name search).
- **#58** — budget cut: valid absence-check (Bajrang Baan never released to radio) but redundant in pattern with kept #61/#63/#64's abundant absence-checking; cut for space.
- **#65** — REDUNDANT (duplicate evidence): identical evidence to kept #56 (ANR mitigation task looks abandoned, fix shipped next day under a different commit); #65 just reframes the same fact as a methodological/meta question. Kept #56.
- **#66** — REDUNDANT (duplicate evidence): identical evidence to dropped #52 (receipt-reconciliation migration, no independent git proof of prod application) — drops for the same underlying reason plus direct duplication.
- **#68** — budget cut / too meta: this is a synthesis question whose own ground truth key is just a comparative recap of #47/#57/#59/#60 (kept as #15/#20/#21/#22 in the final numbering) — already independently present in the final set; adds no new fact of its own.

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

File: /private/tmp/claude-501/-Users-ramakrishnanannaswamy-projects-claude-self-reflect-csr-engine/efab2eb0-47e7-4b91-bb78-a962261c4214/scratchpad/t3-eval/drops.md
