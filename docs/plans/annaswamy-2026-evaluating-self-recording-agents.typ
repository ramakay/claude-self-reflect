// Short paper (arXiv contender, cs.IR primary, cs.SE secondary).
// Receipt paths are repo-relative to the public repository (claude-self-reflect).
// US Letter, single column, 11pt New Computer Modern, justified.

#set document(title: "Evaluating a System That Records Its Own Sessions", author: "Rama Annaswamy")
#set page(
  paper: "us-letter",
  margin: (left: 1.25in, right: 1.25in, top: 1in, bottom: 1in),
  numbering: "1",
)
#set text(font: "New Computer Modern", size: 11pt, lang: "en")
#set par(justify: true, leading: 0.65em)
#set heading(numbering: "1.1")

#show raw: set text(font: "DejaVu Sans Mono", size: 0.9em)
#show heading.where(level: 1): it => {
  v(1.5em, weak: true)
  text(size: 14pt, weight: "bold", it)
  v(0.75em, weak: true)
}
#show heading.where(level: 2): it => {
  v(1.2em, weak: true)
  text(size: 12pt, weight: "bold", it)
  v(0.5em, weak: true)
}
#set figure.caption(position: top)
#show figure.caption: set text(size: 10pt)
#show figure: set block(above: 1.5em, below: 1.5em)

// ---------- Title block ----------
#align(center)[
  #set par(justify: false)
  #set text(hyphenate: false)
  #text(size: 17pt, weight: "bold")[Evaluating a System That Records Its Own Sessions: A Single-Operator Case Study of Contamination, a Benchmark Our Own Method Lost, and a Rerunnable Staleness Test]

  #v(1.2em)
  #text(size: 12pt)[Rama Annaswamy] \
  #text(size: 11pt)[Independent Research]

  #v(0.6em)
  #text(size: 11pt)[November 21, 2026]

]

#v(1.5em)

// ---------- Abstract ----------
#align(center)[#text(size: 12pt, weight: "bold")[Abstract]]
#v(0.6em)
#pad(left: 0.75in, right: 0.75in)[
  #set text(size: 10pt)
  #set par(justify: true)

  // receipt: docs/plans/saga-e3-results.md:5-11, 17-27, 31-32; csr-engine/eval-kit/e3/score.py:34-39; docs/plans/saga-t3-results.md:112-124, 117-120; csr-engine/eval-kit/t4/README.md:43-51, 90-98, 123-129, 148-157
  A coding agent's sessions can be recorded and indexed so that later sessions recall the agent's own past work. Evaluating retrieval over that record is unusual, because the evaluation dialogue is recorded too, and its echoes compete with the material being sought. We report a single-operator case study of how to evaluate such a system, carried by three results whose receipts are public. In a controlled injection at one dose, five scripted re-ask cycles left 4.9 of the top 10 results of exact nearest-neighbour search containing the query verbatim; query-echo defenses cut that verbatim-match occupancy by 72% against the same retrieval walk without them, and a byte-matched sham left nearest-neighbour rankings unchanged. In a pre-registered cross-source benchmark, our own seed-conditioned walk lost exact receipt lookup to a hybrid dense and full-text baseline (recall\@5 0.581 against 0.813 on 396 queries) and did not separate from it on 14 sealed multi-hop questions, an underpowered null; both outcomes held after a contamination vector found later was scrubbed. An offline replay over release tags, which needs no private corpus, scored a stamp-comparison rule at precision 1.000 on 13,626 beliefs, with recall 1.000 by construction, because the rule sees the final tag behind the labels, and no sampled stamp restoration to falsify it. From the failures behind these results we derive seven evaluation controls.
]

#v(0.6em)
#pad(left: 0.75in, right: 0.75in)[
#text(size: 10pt)[*Keywords:* evaluation methodology, evaluation contamination, coding-agent transcripts, rank fusion, pre-registration, staleness, recall over recorded history]
]

#v(1.2em)

// ==================== 1. Introduction ====================
= Introduction

A coding agent works in sessions. Each session leaves a transcript: the operator's prompts, the agent's reasoning and tool calls, the transcripts of its subagents, and its file edits. A system that indexes those transcripts lets a later session recall why earlier work took the shape it did. Rationale questions are among the hardest that developers report (Ko et al., 2007; LaToza & Myers, 2010), and the reasons behind code, what Naur (1985) called the theory of a program, are seldom in the code itself.

Evaluating retrieval over such a record has a property that ordinary retrieval evaluation lacks: the system records the sessions in which it is evaluated. A question asked while the benchmark is designed becomes a transcript, the transcript is indexed, and the indexed question is then the closest match to itself. Asked "why did we drop Qdrant?", a similarity search can rank an earlier session that asked the same question above the session where the decision was made. The same holds for answer generation, for judging, and for any background model call whose transcript the importer picks up. Measurement changes the corpus under measurement.

This paper is a case study of that problem on one operator's recorded history and one system, Self-Reflect (repository `claude-self-reflect`), a local Rust engine that gives a coding agent recall, a recap and staleness verdicts over its own recorded work. Storage is plumbing here, and the system's design is not the contribution. The contribution is method: what an evaluation of a self-recording system has to control, shown through the failures we hit. Every number in this paper comes from a results file, an evaluation kit or a source file in the public repository.

The contributions are four.

// receipt: docs/plans/saga-e3-results.md:5-11, 15-27, 31-32; csr-engine/eval-kit/e3/score.py:34-39 (E3 item); no other numbers in this list
+ Seven evaluation controls for self-recording systems, each tagged observed, tested or proposed (@sec-controls).
+ A controlled injection at one dose (E3): after five scripted re-ask cycles, about half of the top 10 of exact nearest-neighbour search contained the query verbatim, query-echo defenses removed most of that, and a byte-matched sham changed nothing (@sec-e3).
+ A pre-registered cross-source benchmark (T3) in which our own walk lost exact receipt lookup to a hybrid baseline and did not separate from it on sealed multi-hop questions, also after a late-found contamination vector was scrubbed, with a post-hoc, untested hypothesis about why (@sec-t3).
+ An offline replay (T4) of a stamp-comparison staleness rule over release tags, which needs no private corpus (@sec-t4).

// receipt: docs/plans/saga-e3-results.md:5-11
The paper does not claim that the walk beats hybrid retrieval: T3 found the reverse on receipt lookup and could not separate the two on multi-hop questions. It does not claim generality beyond one operator's history, or that the T4 replay tests the auditor of @sec-setting or transfers to the verdicts a deployed system shows. The main contamination result rests on a private collection, eight queries and one scripted dose; a public, rerunnable E3 fixture, not yet built, comes first.

// ==================== 2. Setting ====================
= Setting <sec-setting>

== The recorded history

// receipt: docs/plans/saga-reinstatement-spike.md:20, 24, 72; docs/plans/saga-e2-results.md:10
The corpus is one operator's coding-agent history, from this system's own repository and a second project of the same operator. The importer splits transcripts into chunks, embeds them locally and stores them in SQLite beside a full-text index; subagent transcripts become conversations of their own. A co-edit ledger, the table `code_evolution`, records which sessions touched which files. The walk reads it, and the earliest experiment built its ground truth from it, when the store held 71,762 chunks over 528 conversations. Each later experiment ran on its own frozen snapshot (@sec-results).

== Three retrieval arms

// receipt: docs/plans/saga-reinstatement-spike.md:35-43; docs/plans/saga-t3-preregistration.md:89-92; docs/plans/saga-t3-results.md:142-146; csr-engine/eval-kit/e3/spec.md:51-57, 64
Three arms recur. _One-shot cosine kNN_ returns the top $k$ chunks and stored reflections by cosine similarity to the query; it has no lexical channel. _Hybrid retrieval_, the K arm of T3, fuses a vector top-20 and a full-text top-20 by reciprocal-rank fusion with score $sum 1 slash (60 + "rank")$, removes duplicates and truncates to the shared budget (Cormack et al., 2009). The _reinstatement walk_ (the tool `csr_why`) is our own. It takes the top 3 kNN results as seeds. For each seed vector $s$ it searches again with $"renorm"(0.65 q + 0.35 s)$, a per-seed analogue of pseudo-relevance feedback (Rocchio, 1971) and of context reinstatement in models of recall (Howard & Kahana, 2002); it spreads from the seed's session to other sessions that touched the same files; and it follows one step back along a chain of episode summaries. It then fuses, removes duplicates and keeps $k = 10$. No language model takes part.

// receipt: docs/plans/saga-t3-results.md:142-146; csr-engine/src/search/reinstatement.rs:360; csr-engine/src/search/rerank.rs:44-47; csr-engine/eval-kit/e3/spec.md:54-60, 64
The walk carries three defenses against its own echoes. _Echo-aware seed selection_ prefers seeds that do not contain the query verbatim. A _query-echo demotion_ subtracts 0.35 from a chunk that quotes the query. A _scaffold demotion_ subtracts 0.30 from text the ranker classifies as machine-assembled scaffolding. In E3 a chunk is an echo when its text contains the full query, case-insensitively, whatever its source.

== Deterministic witness verdicts

// receipt: codewitness/README.md:1-31; csr-engine/eval-kit/t4/README.md:24-28
The system also attaches staleness verdicts to evidence bound to code. The public crate `codewitness` stamps an anchor (a file, a symbol or a span) into a witness, hashed from content in the worktree or at a given commit, and later audits it to exactly one verdict: _Intact_, _Drifted_, _Superseded_ (with a receipt from a committed successor) or _Vanished_. No language model writes or judges a verdict. Causal order comes from git ancestry, never from timestamps, and an A$arrow$B$arrow$A revert returns to _Intact_, because content identity beats history. T4 does not exercise this auditor (@sec-t4).

// receipt: docs/plans/saga-reinstatement-spike.md:72; docs/plans/saga-t3-results.md:3; csr-engine/eval-kit/h1/bootstrap.py:13
The released system differs from the research builds used here in ways no experiment measures, among them a general recall path that fuses dense, full-text and identifier rankings (`csr-engine/src/search/hybrid.rs`).

// ==================== 3. Related work ====================
= Related work

*Contamination of evaluation by retrieved or self-authored material.* Han et al. (2025) show search-capable agents retrieving benchmark answers from the open web at test time. Druck and Smith (2026) show, in recursive simulations, responses losing diversity when the retrieved documents are self-authored. Sung et al. (2026), the closest precedent for E3, vary query-targeted poisoning budgets against matched controls, and Fröbe et al. (2022) document train-test leakage in retrieval test collections; in E3, by contrast, routine evaluation dialogue enters the index through the system's own recording pipeline.

*Relevance feedback and query drift.* The walk's blend step is close to Rocchio's (1971) relevance feedback and its pseudo-relevance variants, and we claim no novelty for it. Their known failure is query drift: expansion from pseudo-feedback documents can shift the query away from its original intent (Zighelnic & Kurland, 2008). Deng (2026) measures lexical pseudo-relevance feedback as a null on intent-style queries over agent histories, with regressions from drift. In a self-recording corpus the top-ranked documents include restatements of the query, so drift goes toward the system's own echoes; the echo demotions are drift countermeasures for that case. Expansion at query time from seed nodes over a graph, as in HippoRAG 2 (Gutiérrez et al., 2025), is a second relative of the walk.

*Rank fusion.* Reciprocal-rank fusion (Cormack et al., 2009) is a generic rule for fusing ranked lists with almost no tuning; T3 applies it to dense and lexical rankings, and that fused baseline beat the walk. Xue et al. (2026) find that the dense advantage over lexical search in code search depends on evaluation choices and on queries without rare identifiers. T3 is a neighbouring observation: its lexical channel alone was weak.

*Staleness and validity of stored agent context.* Concord (Liu et al., 2026) binds tool observations to their mutable sources and updates, annotates or suppresses stale context. LEDGER (Islam et al., 2026) lets a model propose facts while explicit rules decide each fact's current value. MemTrace (Xu et al., 2026) checks file-, symbol- and test-anchored execution records against the repository before reuse, with gains on downstream coding benchmarks; it is the closest precedent for witness verdicts and has what T4 lacks, an end-task evaluation. Xiao et al. (2026) derive verifiable temporal tasks from repository history with cutoffs on what an agent can see, which T4 lacks.

*Evaluation method for retrieval over agent histories.* Deng (2026) sets a standard for attributing retrieval lift, with pinned BM25 floors and paired bootstrap intervals; Fröbe et al. (2023) frame blinded, reproducible IR evaluation, the setting of C6. CounterMem (Baklanov et al., 2026) tests whether answers change when only the retrieved context changes; Soei (Khan & Kumar, 2026) separates an agent's completion claims from proof. We add the self-recording problem: the evaluation must be kept out of the corpus it evaluates, and what leaks in must be measured.

*Project records mined from software artifacts.* Hipikat (Čubranić et al., 2005) built a project record from source, issues, mail and documents. With coding agents the record writes itself, and its own growth can displace what is sought.

// ==================== 4. Method: controls ====================
= Method: controls for evaluating a self-recording system <sec-controls>

Each control names a failure, where its evidence sits, and a tag: \[observed\] (a run showed the failure), \[tested\] (a run measured the control) or \[proposed\] (no run has validated it). None is new in isolation.

// receipt: docs/plans/saga-e3-results.md:5-6, 33-34; docs/plans/saga-t3-preregistration.md:41-50, 106-108; docs/plans/saga-relitigation-results.md:22-24
*C1. Freeze a snapshot before the evaluation dialogue can be recorded.* \[observed\] The failure: a session that designs the evaluation is indexed and competes with the origins it asks about. In E3 the evaluation-design transcript alone entered one query's top 3 (@sec-e3). The control is a session-zero snapshot, taken before any evaluation dialogue exists, or an explicit exclusion of every construction session, as T3 did for 119 conversations and the re-litigation benchmark for its design session and subagents. A freeze taken after the discussion on the same day is not enough, and no run has shown an exclusion rule exhaustive.

// receipt: docs/plans/saga-e1-results.md:3-4, 42-44; docs/plans/saga-e3-results.md:3-5
*C2. Share one index build across compared arms.* \[observed\] The failure: approximate nearest-neighbour recall is sensitive to build conditions (Elliott & Clark, 2024). The same kNN baseline scored origin-MRR 0.243 on the channel ablation's build of a frozen snapshot and 0.193 on the graded-gold comparison's build two days earlier, under a different harness too, so only comparisons within one build are claimed. The control is one stamped index build per comparison, or exact brute-force scan, as in E3.

// receipt: docs/plans/saga-reinstatement-spike.md:36-37, 76-79; docs/plans/saga-t3-results.md:112-115, 132-137
*C3. Give the baseline a lexical channel.* \[tested\] The failure: the walk's first win (@sec-support) was against one-shot cosine kNN, which has no lexical channel. Given reciprocal-rank fusion with full-text search, the T3 baseline beat the walk on receipt lookup, though lexical search alone was weak (@sec-t3). The control is a baseline built from the cheapest strong retrievers, dense, lexical and their fusion, with an ablation of its own channels, so that a win or a loss can be traced to a channel.

// receipt: docs/plans/saga-e3-results.md:7-8, 31-32; docs/plans/saga-t3-results.md:139-148; csr-engine/eval-kit/h1/bootstrap_results.json (S_A-S_Asham)
*C4. Measure echo handling per query class, and include a sham.* \[proposed\] The failure is a hypothesis: a post-hoc trace suggests that the echo defenses that repair conversational queries in E3 cause the walk's misses on exact receipt lookups in T3 (@sec-t3). A defense tuned on one query class may invert on another, so its effect has to be reported per class. A sham separates a content effect from a size or density effect: in E3 an unrelated transcript matched in raw bytes, and in the channel follow-up a graph with the same degrees and shuffled edges (@sec-support).

// receipt: docs/plans/saga-t3-results.md:61-71, 90-124
*C5. Scrub self-generated transcripts, isolate the answering model, and report both runs.* \[observed\] The failure: after T3’s results were committed, we found that the system's own nested model runs had written transcripts that the importer indexed, and that they made up most of the snapshot's conversations (@sec-t3). Earlier in the same run, the operator's persistent notes file had loaded into the answering model, and the no-recall arm cited answer material from it. The controls are a classifier for self-generated transcripts run before freezing (not yet validated), a clean-room answering environment with settings, tools and hooks cut, and, when contamination is found late, a scrubbed re-run reported beside the original.

// receipt: docs/plans/saga-t3-preregistration.md:1-4, 110-118; docs/plans/saga-ratification-results.md:26-46
*C6. Pre-register gates and commit the failure interpretation in advance.* \[observed\] The failure guarded against is the easiest to commit: reading a loss as a mixed result. T3’s pre-registration stated, before any arm ran, what each combination of gate outcomes would mean (@sec-t3). A registered gate also makes a correction cheap to check: when review found that a ratification extractor had silently used the wrong input, the gate was re-run with the fix and the null became flatter (@sec-support). Results should be committed with their per-run outputs. Here every reported result has a committed results file, and the reproducibility section says which per-run outputs are also released and which are not.

// receipt: csr-engine/eval-kit/t4/README.md:1-9, 43-51, 131-146, 163-173; codewitness/src/bin/codewitness.rs:802-815
*C7. For staleness, build labels from repository history alone.* \[tested\] The failure: hand-made labels on a private corpus cannot be checked by anyone else, and a scorer written by the method's author can favour the method. T4 derives predictions and ground truth from release tags, scores every arm with one scorer, reruns given the endpoint tags and the stamper's extractor, and audits directed to look for rigging found two defects in the method's favour before the numbers were admitted (@sec-t4). Its rule still sees the final tag behind the labels.

// ==================== 5. Results ====================
= Results <sec-results>

== Controlled contamination (E3) <sec-e3>

// receipt: docs/plans/saga-e3-results.md:1-11, 31-32, 46-49; docs/plans/saga-e2-results.md:41-43, 83; csr-engine/eval-kit/e3/spec.md:28-38, 50-60, 64, 67-75; csr-engine/eval-kit/e3/queries.txt
E3 (run 2026-07-17) asks how much recorded evaluation dialogue displaces the origin of a decision, and whether the walk's echo defenses repair it. The base condition C0 is a backup of 17,134 chunks that pre-dates all evaluation dialogue. C1 adds only the transcript of the session that wrote the queries (265 chunks). C-sham adds one unrelated transcript matched to C1 by raw bytes (107.3%), with zero occurrences of any query text (167 chunks, against C1's 265). C5 adds five scripted re-ask cycles, marked synthetic, each asking every query verbatim with a paraphrased answer (16 chunks); it is a controlled injection at one dose, compared with zero, not an observation of natural use. Retrieval used exact brute-force scan, so corpus content is the only variable. Eight queries were eligible, because their origins pre-date C0; the E3 receipt calls all eight origin maps owner-audited, while the graded-gold receipt documents five, four of them on the E3 roster. Three arms ran in every condition: exact kNN and the walk with and without two echo switches, echo-aware seed selection and the query-echo demotion, toggled together; both walk arms kept the provenance reranker and its scaffold demotion.

// receipt: docs/plans/saga-e3-results.md:15-21; csr-engine/eval-kit/e3/spec.md:50-64; csr-engine/eval-kit/e3/score.py:38-39
#figure(
  table(
    columns: (2.4fr, 1fr, 1fr),
    stroke: none,
    align: (left, center, center),
    table.hline(),
    table.header([*Arm, condition C5 (dosed re-asking)*], [*echo\@10*], [*origin-MRR*]),
    table.hline(),
    [Exact kNN], [4.9 / 10], [0.014],
    [Walk, both echo switches off], [4.9 / 10], [0.014],
    [Walk, both echo switches on], [1.4 / 10], [0.047],
    table.hline(),
  ),
  caption: [E3 under dose: mean over 8 eligible queries, exact brute-force retrieval. echo\@10 counts top-10 chunks containing the full query, from any source. The walk arms differ only in the two echo switches.],
) <tab-e3>

// receipt: docs/plans/saga-e3-results.md:23-30, 46-49; csr-engine/eval-kit/e3/score.py:34-39, 58-62; csr-engine/eval-kit/e3/spec.md:62-64
Under C5, a mean 4.9 of exact kNN's top 10 contained the query verbatim (@tab-e3), and for one query the whole kNN top 3 was synthetic cycles. This is verbatim-match occupancy from any source, not a count of injected chunks. The walk without its defenses was captured to the same degree, because its second hop spreads outward from echo seeds. With the defenses on, echo occupancy fell to 1.4, a 72% reduction against the same walk with the defenses off under the same dose, and origin-MRR rose from 0.014 to 0.047, 3.4 times as high. On one query the origin entered the top 10, at rank 8, only in the defended arm under C5.

// receipt: docs/plans/saga-e3-results.md:31-34
This particular sham, one unrelated transcript matched to C1 by raw bytes, left the exact-kNN rankings byte-identical to C0; it does not show that corpus size could never matter. C1 shows the self-recording problem at its smallest: the one stored evaluation-design conversation inserted itself into one query's kNN top 3 and moved the walk's rank for another query's origin from 3 to 4.

// receipt: docs/plans/saga-e3-results.md:15-21, 26, 36-42, 46-50, 55; csr-engine/eval-kit/e3/ (no per-query output files)
Under exact scan on this early corpus, origins sit outside the top 10 for 6 to 7 of the 8 queries in every condition, so origin-rank trajectories are floor-limited and observable on two queries only; echo occupancy and the repair under dose are the load-bearing results. There is one dose against zero, one corpus, and no intervals, and the receipt reports the sham's equality for exact kNN only. The three means imply 89 echo slots, but the receipt counts 86 echo hits under C5 without saying why, and per-query counts are not public. The re-asks repeat the query verbatim and the echo test is verbatim containment, while natural re-asks paraphrase, so 72% is measured on the easiest echoes to detect.

== Cross-source benchmark: the walk loses receipt lookup (T3) <sec-t3>

// receipt: docs/plans/saga-t3-preregistration.md:1-25, 87-99; docs/plans/saga-relitigation-results.md:61-68
The re-litigation benchmark (@sec-support) found the walk and one-shot kNN tied on single-hop tasks, which left a narrower hypothesis: that any advantage of the walk lies in provenance questions whose evidence spans several sources. T3 tested it under a protocol committed on 2026-07-27 before any arm ran. Three arms answered over one frozen snapshot: R, the walk, with a resolution ledger of recorded verdicts loaded; K, the hybrid arm of @sec-setting; and N, a no-recall arm that sees only the question. K's lexical channel was a fairness grant written into the protocol, so commit hashes in chunk text are searchable for K and only typed edge traversal is exclusive to R.

// receipt: docs/plans/saga-t3-final-set.md:6-8; docs/plans/saga-t3-drops.md:1-6, 54-62; docs/plans/saga-t3-preregistration.md:29-39, 55-60, 100-108; docs/plans/saga-paper-draft.md:241; csr-engine/eval-kit/t3/mech_pairs.frozen.csv (columns repo, receipt_conversations); csr-engine/eval-kit/t3/SEAL.sha256
Two sets were frozen. The _deep set_ is 27 sealed questions, kept from 68 git-grounded candidates by four filters and a balance pass, with each of the 41 drops logged. Fourteen are multi-hop; 11 come from this system's repository and 16 from a second project of the same operator. The question writers had no access to the system's tools or database, and every key derives from git history, plan files and task lifecycles. The _mechanical set_ starts from 529 commit messages in four source repositories; 509 hashes appear in indexed conversation text, 423 inside the result of the git command itself (receipt-class) and 86 only in prose (citation-class). Gold is the set of receipt conversations, and a hit is any member in the top 5. Receipt-class gold was the pre-specified primary; 27 pairs fell inside the freeze exclusion of 119 construction conversations, which left 396. For 181 of these the set has more than one conversation, so a git tool result marks a receipt match, not a unique authoring session; no sensitivity analysis for this was run.

// receipt: docs/plans/saga-t3-preregistration.md:55-76, 110-118; docs/plans/saga-t3-results.md:40-44, 61-74
_Gate M_ scores recall\@5 of the receipt-class gold, with no judge. _Gate D_ scores the deep set with two judges from other model families (the Codex and Grok command-line agents), blind to arm; an answer is scored only when both agree, and a split drops the question from that arm; no sensitivity analysis for splits was run. Each gate passes if R beats K by a sign test over discordant pairs at $p < 0.05$, for Gate D on the multi-hop subset; both gates were primary. The registration fixed direction and threshold, not sidedness; we report two-sided exact tests. Answers came from one model (Sonnet) under one prompt scaffold. The first answer batch loaded the operator's persistent notes file into all three arms; it was caught before any judging, deleted and regenerated in a clean room.

// receipt: docs/plans/saga-t3-results.md:90-110
_Contamination found after the results were committed._ A hook-recursion bug, since fixed, had let about 4,800 nested headless model runs write transcripts that the importer indexed. The frozen snapshot held 4,346 of them, 84% of its 5,187 conversations, and removing them cut it from 154,389 to 97,861 chunks; the freeze exclusion missed them because they pre-date its cutoff. On the original run they filled 9.5% of R's Gate M top-5 slots against 5.1% of K's, and about 14% of both arms' Gate D context items. A classifier removed them, Gate M was re-run in full, and K and R regenerated their Gate D answers; no-recall answers, which read no retrieved context, were reused. @tab-t3 reports both runs.

// receipt: docs/plans/saga-t3-results.md:22-30, 44-49, 112-120
#[
#set text(size: 9.5pt)
#set par(justify: false)
#figure(
  table(
    columns: (1.75fr, 0.95fr, 0.55fr, 0.8fr, 0.8fr, 1.35fr, 1.25fr),
    stroke: none,
    align: (left, left, center, center, center, center, center),
    table.hline(),
    table.header([*Gate (n)*], [*Run*], [*N*], [*K*], [*R*], [*R-only / K-only*], [*$p$*]),
    table.hline(),
    [M, receipts (396)], [original], [n/a], [0.783], [0.545], [13 / 107], [$1.5 times 10^(-19)$],
    [M, receipts (396)], [scrubbed], [n/a], [0.813], [0.581], [11 / 103], [$6.9 times 10^(-20)$],
    table.hline(stroke: 0.5pt + rgb("#cccccc")),
    [D, multi-hop (14)], [original], [0/14], [6/13], [3/13], [0 / 2], [0.50],
    [D, multi-hop (14)], [scrubbed], [0/14], [4/12], [3/13], [0 / 2], [0.50],
    [D, full set (27)], [original], [0/27], [13/24], [10/24], [1 / 2], [1.00],
    [D, full set (27)], [scrubbed], [0/27], [11/22], [10/25], [0 / 4], [0.125],
    table.hline(),
  ),
  caption: [T3 gate results. Gate M is recall\@5 of the receipt set. Gate D counts answers both judges call correct, over the questions with a consensus verdict for that arm (splits: 6 of 81 original, 7 scrubbed); a discordant pair needs a verdict in both arms. "Scrubbed" is the re-run after the residue was removed, with no-recall answers reused. Sign tests are two-sided and exact; Gate M $p$ values are approximate. N = no recall, K = hybrid vector and full-text, R = reinstatement walk.],
) <tab-t3>
]

// receipt: docs/plans/saga-t3-results.md:22-30, 112-115, 122-124
_Gate M failed in the reverse direction._ On the original snapshot, R placed a receipt conversation in its top 5 for 216 of 396 queries and K for 310, with 13 R-only against 107 K-only pairs. On the scrubbed snapshot R scored 0.581 and K 0.813, with 11 against 103 (@tab-t3). Scrubbing raised both arms and changed no conclusion.

// receipt: docs/plans/saga-t3-results.md:44-54, 117-124, 153-155; docs/plans/saga-t3-results.md:17-19
_Gate D is an underpowered null._ On the 14 multi-hop questions R answered 3 correctly in both runs, and K 6 in the original run and 4 in the scrubbed one, with discordant pairs 0 against 2 ($p = 0.50$). The direction mildly favours K, and with two to four discordant pairs the arms cannot separate either way. The no-recall arm scored zero of 27 in the observed generation run, which supports a benefit of retrieved context under the tested settings and says nothing in favour of the walk.

// receipt: docs/plans/saga-t3-results.md:126-146
_Component ablation and failure trace (post-hoc)._ After the gates were known we split K into its channels on the scrubbed snapshot: full-text alone scored 0.162, vector alone 0.697, and their fusion 0.813. The walk, at 0.581, scored below the vector channel that seeds it. A code trace of the 103 queries that only K answered proposes two hypothesised causes. Echo-aware seed selection avoids seeds that contain the query verbatim, but on receipt lookup the chunk that contains the commit message verbatim is the gold, so the second hop spreads from the wrong sessions; the trace assigns 63 of the 103 to this (gold absent from the walk's top 10). The query-echo and scaffold demotions stack on receipt-shaped chunks; the trace assigns the other 40 to this (gold at rank 6 to 10). Echo-aware seed selection and the query-echo demotion are the two switches E3 toggled together. The trace is a post-hoc proposal, and no intervention has tested it: no run disabled either defense on the mechanical set.

// receipt: docs/plans/saga-t3-preregistration.md:110-118; docs/plans/saga-t3-results.md:9-15
_The pre-committed interpretation applies._ Gate M failed and Gate D did not pass. The pre-registration said, before any arm ran, what that outcome means: the walk's multi-hop advantage is not supported on this corpus, and the claim reduces to the provenance rerank, the ledger's hygiene and the receipts layer.

// receipt: docs/plans/saga-t3-results.md:40-44, 153-155; docs/plans/saga-t3-preregistration.md:62-66, 73-74
_Limits._ One operator's history from two project groups, with the mechanical set from four source repositories. One answering model family, so Gate D measures what retrieved context does for that family. Two judges, so splits could not be settled by majority as the protocol planned. Gate D contexts cut each retrieved item to a 200-character excerpt, which often truncates multi-hop evidence for both arms. Gate M gold is built from commit hashes, with the commit message as the query, which favours an arm with a lexical channel.

== Release-tag staleness benchmark (T4) <sec-t4>

// receipt: csr-engine/eval-kit/t4/README.md:11-51, 103-121; codewitness/README.md:9-21; codewitness/src/bin/codewitness.rs:621-640, 752-773
T4, an offline replay over release tags, asks whether the witness rule, a stamp comparison, predicts which recorded beliefs about code are stale, using labels the repository gives for free. A _belief_ is a witness stamp of one (file, symbol) pair at a release tag. Between `v8.0.0` and `v9.5.0` the repository has 17 `v*` tags; 13 are sampled by an integer linspace that includes both endpoints, and every pair stamped at one of the 11 intermediate tags is a belief, 13,626 in all. The witness rule predicts superseded if any later sampled tag holds a different stamp for the key, and obsolete if the key is absent at the final tag. Ground truth is read directly at the final tag: obsolete if absent, superseded if the stamp differs. Stale means either. Unlike the auditor of @sec-setting, which returns _Intact_ after A$arrow$B$arrow$A, the rule flags such a belief stale if a sampled intermediate tag holds B. _Grep_ predicts stale when the symbol's name is not a substring of its file at the final tag (read with `git show`) or the file is gone. _Recency-N_ predicts stale when the belief's tag was committed more than N days before the final tag. All arms share one ground-truth vector and one scorer, with no arm-specific filtering and no language model.

// receipt: csr-engine/eval-kit/t4/README.md:123-129; csr-engine/eval-kit/t4/results.json (arm_metrics)
#figure(
  table(
    columns: (1.9fr, 0.8fr, 0.75fr, 0.9fr, 0.8fr, 1.15fr, 0.9fr, 0.7fr),
    stroke: none,
    align: (left, center, center, center, center, center, center, center),
    table.hline(),
    table.header([*Arm*], [*TP*], [*FP*], [*TN*], [*FN*], [*Precision*], [*Recall*], [*F1*]),
    table.hline(),
    [Witness rule], [1,265], [0], [12,361], [0], [1.000], [1.000], [1.000],
    [Grep], [20], [0], [12,361], [1,245], [1.000], [0.016], [0.031],
    [Recency 30 d], [920], [5,677], [6,684], [345], [0.139], [0.727], [0.234],
    [Recency 90 d], [556], [3,020], [9,341], [709], [0.155], [0.440], [0.230],
    [Recency 180 d], [0], [0], [12,361], [1,265], [0.000], [0.000], [0.000],
    table.hline(),
  ),
  caption: [T4 replay of 13,626 beliefs from 11 intermediate release tags, run at `5d2bd81`; the positive class is stale. TP, FP, TN, FN: true and false positives, true and false negatives, in the receipt's order. The witness rule's recall is guaranteed by construction, so precision is the comparison column.],
) <tab-t4>

// receipt: csr-engine/eval-kit/t4/README.md:43-51, 90-98, 148-158, 181-192, 210-219; csr-engine/eval-kit/t4/results.json (tag_commit_timestamps)
_Why recall is 1.000 by construction._ The witness rule's scan of later tags includes the final tag behind the labels, so its predicted-stale set contains the ground-truth stale set. Recall, and with it F1, favour the rule by design, and precision is the one number with empirical content (@tab-t4). Precision can fall below 1.000 only through a stamp that changes at an intermediate tag and returns to its original value by the final tag. No sampled stamp did so here (Tier A), and Tier B's commit-subject detector labels none of 558 reachable commits a revert: precision 1.000 is unfalsified, not proven, and a held-out final tag with a known change-and-restoration case is the missing test. Grep is precise but blind: its 20 stale predictions are all correct, and it finds 20 of 1,265 stale beliefs. Recency is weak at every threshold (F1 at most 0.234), and at 180 days it predicts nothing stale, because no intermediate tag is that much older than the final tag.

// receipt: csr-engine/eval-kit/t4/README.md:131-146
_Two defects caught by audit before the numbers were admitted._ Audits directed to look for rigging in the rule's favour found it twice. The grep arm handled `::` qualification but not dot qualification; under symmetric handling the grep arm has no false positives, so neither arm has any and there is no precision gap between them. The extractor had collapsed coexisting same-named definitions in one file, such as two `is_empty` methods on different types, into one key, corrupting prediction and ground truth alike; extraction now keys by node, span and AST ordinal, which moved the beliefs from 13,575 to 13,626 and the stale count from 1,275 to 1,265. @tab-t4 uses the corrected extractor.

// receipt: csr-engine/eval-kit/t4/README.md:53-63, 163-173, 175-221; codewitness/src/bin/codewitness.rs:180-185, 236-237, 291-299, 589-604, 802-815; csr-engine/src/extraction/ast_analysis.rs:393-403
_How to rerun it._ `codewitness bench --repo <path> --binary <csr-engine> --scratch-dir <dir>` stamps every sampled tag into a scratch database and scores all arms; `--first-tag` and `--last-tag` choose the endpoints, which must be `v*` tags, and only code the stamper can parse yields beliefs. Back-to-back reruns give identical metrics, and each result records the benched HEAD, the stamper binary's SHA-256 and per-tag stamping statistics, so an empty or collision-blind run is visible. A second tier, `codewitness labels`, labels commits shipped, reverted or unreleased from tag ancestry (140 tags, 540 of 558 commits shipped, none detected as reverted by commit subject); its positive class is empty here, so it supplies coverage only, and its optional session linkage reads the operator's private store.

== Supporting public results <sec-support>

// receipt: docs/plans/saga-reinstatement-spike.md:32-59, 72-86
*The first coverage win, scoped.* In a pre-registered spike on 2026-07-15, 12 why-questions were each paired with a target file, and ground truth was the set of sessions the co-edit ledger records as touching it. At a budget of 10 the walk covered 23 ground-truth sessions against 15 for one-shot cosine kNN, +53% against a gate of +25%, with 7 per-query wins, 5 ties and no losses. The baseline had no lexical channel, and the ground truth shares the ledger that the walk's graph step reads.

// receipt: docs/plans/saga-e2-results.md:10, 22-28, 34-45, 81-85
*Graded gold.* Graded origin labels were then built for 20 queries (12 on this system's history, 8 on a second project of the same operator) from the operator's dialogue acts and ship records, with origins sealed before any ranks were seen; 12 queries have a mapped origin. On those 12 the walk led kNN on origin-MRR (0.264 against 0.193), `nDCG@10` (0.470 against 0.363) and recall\@10 of grade-2-or-better items (0.553 against 0.444), as point estimates without intervals. Only the five injected-origin maps were owner-audited, and all five held; the vendor-split items and grade spot-checks were not audited. Both arms miss many of the verified origins at $k = 10$.

// receipt: csr-engine/eval-kit/h1/bootstrap_results.json (S_F-S, S_A-S, S_A-S_Asham); csr-engine/eval-kit/h1/bootstrap.py:13, 25-26; csr-engine/eval-kit/h1/results.json (meta.ast_real_edges); docs/analysis/2026-07-31-codegraph-user-impact-proof.md:20, 304
*Sham-controlled channel follow-up.* A five-arm grid on the same sealed gold and one index build varied only the expansion channel, with 10,000-resample paired bootstrap intervals (seed 20260731). Co-edit spread over no expansion gained +0.0167 origin-MRR on 12 queries, 95% CI [0.000, 0.050], and +0.0264 `nDCG@10` on 20, CI [$minus$0.0058, 0.0713]: not separable from zero. Spread over the AST symbol graph (4,092 resolved edges) matched the base arm's origin-MRR exactly and differed from a degree-preserving edge-shuffle sham by +0.0002 `nDCG@10`, CI [$minus$0.0132, 0.0139]. That null is bounded: the AST channel fired on 8 of 20 queries, and its seeds were mapped through an attribution field that agreed with edit events 50.74% of the time.

// receipt: docs/plans/saga-ratification-results.md:26-46, 48-56
*A pre-registered null.* A global per-conversation ratification score, from dialogue acts a small model extracted, was gated on Spearman correlation with the sealed grades, with a halt at $rho approx 0$. It halted three times: $rho = 0.060$ ($n = 121$), $0.071$ ($n = 123$) and, after review found that the second extractor had silently sampled the wrong turns and the fix was applied, $0.036$ ($n = 111$). The score is global; relevance to a decision is conditional on the query.

// receipt: docs/plans/saga-relitigation-results.md:10-50, 70-74
*Re-litigation, in rubric points.* Sixteen sealed tasks asked an agent to act on requests against settled history, in four classes with a pre-registered correct action; three arms differed only in retrieved context (walk, one-shot kNN, no recall). The author graded the 36 responses to the 12 headline tasks on a 0 to 3 rubric, blind to arm. On settled-for-cause tasks the walk scored 10 of 12 points, kNN 9 and no recall 6. On should-change tasks every arm scored 6 of 12; the receipt's response-category counts are internally inconsistent and are not reported. The walk and kNN chose identical actions on all 16 tasks and tied on the total, 27 of 36 each, against 24 for no recall.

// ==================== 6. Discussion ====================
= Discussion

// receipt: docs/plans/saga-e3-results.md:23-24, 46-47; docs/plans/saga-t3-results.md:61-71, 92-97
*What a reader should take for their own evaluation.* The system's own activity is part of the corpus and has to be measured as such. E3 shows how little it takes, and T3 shows where it hides: not in the evaluation dialogue, which the freeze excluded, but in the system's own background model runs and the answering model's environment. A protocol has to enumerate every path by which the system writes into its own corpus, and @sec-controls is our current list, not a complete one.

*The trade between contamination defense and exact lookup.* E3 and T3 may point at one mechanism from opposite sides. The bundle of echo-aware seed selection and query-echo demotion repairs retrieval when the corpus is full of re-asks; a post-hoc trace hypothesises that it also made the walk miss commits whose message is the query. If so, no single echo policy suits both query classes, and the design to test is routing by query class: exact-key queries to fused dense and lexical retrieval or a typed join on commit identifiers, conversational why-questions to a path with echo defenses. The test is a Gate M re-run with each switch off in turn.

// receipt: csr-engine/eval-kit/t4/README.md:11-51, 90-98, 210-219; codewitness/README.md:9-21
*Why a benchmark population is not a deployed population.* T4 replays a stamp-comparison rule, not the auditor, on release-tag symbol stamps. That population is clean: each belief hashes committed code at a tagged commit, no sampled stamp restoration occurred, and ground truth uses the same stamper and final tag as the prediction. A deployed system shows verdicts on claims made in sessions, bound to code through attribution, on repositories that do revert. Precision 1.000 here leaves the rule unfalsified on clean inputs and says nothing about the auditor or what a delivered verdict is worth; a held-out final tag with a change-and-restoration case and an end-task evaluation such as MemTrace's (Xu et al., 2026) are the missing tests.

// ==================== 7. Limitations ====================
= Limitations

// receipt: docs/plans/saga-e3-results.md:5-11; docs/plans/saga-e2-results.md:22; docs/plans/saga-t3-results.md:48, 153-155; docs/plans/saga-relitigation-results.md:12
Every result comes from one operator's recorded history in two project groups, and nothing establishes that another operator's history behaves the same way. The main contamination result, E3, rests on a private collection, eight queries and one scripted dose, and the other sets are small too (12 mapped-origin queries, 14 multi-hop questions in T3, 16 re-litigation tasks), and most point estimates carry no interval. The spike's ground truth shares the co-edit ledger with the walk; the graded gold is partly audited; the re-litigation grades come from the author, blind to arm but not independent. All experiments ran on research builds; none compares the released recall path with the walk or measures verdicts as delivered to sessions. The first item of future work is a public, rerunnable E3 fixture with several doses, paraphrased re-asks, matched controls, independent origin labels and paired query-level results for every arm.

_Labelled note on unreleased audits._ Further audits on the operator's own logs exist; they derive from private transcripts, are not released and are not used here.

= Reproducibility and data availability

// receipt: docs/plans/saga-e3-results.md:55; docs/plans/saga-t3-results.md:3-7; csr-engine/eval-kit/t3/ (four files: pairs, sealed questions, seal, task inputs); codewitness/src/bin/codewitness.rs:291-299, 589-604, 802-815
The corpus is one operator's private development history and cannot be released. All receipt paths refer to the public repository at commit 19a26432 (#link("https://github.com/ramakay/claude-self-reflect/tree/19a26432")[github.com/ramakay/claude-self-reflect, tree 19a26432]); @tab-repro lists what is released per experiment. E3's per-query output is not released, so E3 is a protocol to run on one's own corpus. The T3 kit holds inputs and seals but no per-arm hit vectors, judge labels or paired outputs, and its gate tables need the private frozen snapshot. T4 reruns from scratch given `v*` endpoint tags and code the stamper can parse. The channel follow-up's intervals recompute from the released per-query results. A replication of E3 and T3 on another self-recording system is the external test this case study lacks. A longer version of this paper, with the system description and further detail, is `docs/plans/annaswamy-2026-similarity-drowns-intent.pdf` in the same repository.

#[
#set text(size: 9pt)
#set par(justify: false)
#figure(
  table(
    columns: (1.15fr, 1fr),
    stroke: none,
    inset: (x: 4pt, y: 3.5pt),
    align: (left, left),
    table.hline(),
    table.header([*Released (repository path; what it reproduces)*], [*Not released (reason)*]),
    table.hline(),
    [`eval-kit/e3/`: condition spec, re-ask generator, queries, scorer; no per-query output. The E3 protocol, on one's own corpus.], [Corpus databases and transcripts, including the session-zero backup and T3 snapshots: private history.],
    [`eval-kit/t3/`: sealed roster and seal, frozen mechanical pairs, task inputs; no hit vectors or judge labels. T3’s sets and Gate M, given a snapshot.], [Digests and raw extractions behind the graded gold: transcript text.],
    [`eval-kit/t4/` with the `codewitness` crate: `results.json`, `labels.json`. T4 given `v*` endpoint tags.], [T4 session linkage: reads the operator's private store.],
    [`eval-kit/h1/`: per-query results, scorer, bootstrap. The channel follow-up's intervals.], [Commit-subject ledger: spans private repositories.],
    [`eval-kit/e2/`, `e1/`, `relitigation/`: graded-gold identifiers and grades, ablation scorer, sealed tasks.], [Operator-local audit files: derive from private transcripts.],
    [Results files `docs/plans/saga-*.md` for every experiment above.], [Per-run outputs of E3 (per-query counts) and T3 (per-arm hit vectors, judge labels, paired outputs): not in the public kit.],
    table.hline(),
  ),
  caption: [What is released and what is not. `eval-kit/` is `csr-engine/eval-kit/`; paths are relative to the root of the `claude-self-reflect` repository.],
) <tab-repro>
]

// ==================== References ====================
#v(1em)
#text(size: 14pt, weight: "bold")[References]
#v(0.75em)

#set text(size: 10pt)
#set par(justify: false)

Baklanov, M., Barkan, O., & Koenigstein, N. (2026). CounterMem: Counterfactual evaluation of non-parametric memory use in retrieval-augmented generation. _Findings of the Association for Computational Linguistics: EMNLP 2026_, 22332–22348.

#v(0.5em)
Cormack, G. V., Clarke, C. L. A., & Büttcher, S. (2009). Reciprocal rank fusion outperforms Condorcet and individual rank learning methods. _Proceedings of SIGIR 2009_, 758–759.

#v(0.5em)
Čubranić, D., Murphy, G. C., Singer, J., & Booth, K. S. (2005). Hipikat: A project memory for software development. _IEEE Transactions on Software Engineering, 31_(6), 446–465.

#v(0.5em)
Deng, Y. (2026). Entity-collision: A stratified protocol for attributing retrieval lift in agent memory. _arXiv preprint_ arXiv:2605.29630.

#v(0.5em)
Druck, G., & Smith, E. (2026). RAG collapse: LLM responses collapse when retrieved documents are self-authored. _arXiv preprint_ arXiv:2608.22118.

#v(0.5em)
Elliott, O. P., & Clark, J. (2024). The impacts of data, ordering, and intrinsic dimensionality on recall in hierarchical navigable small worlds. _arXiv preprint_ arXiv:2405.17813.

#v(0.5em)
Fröbe, M., Akiki, C., Potthast, M., & Hagen, M. (2022). How train–test leakage affects zero-shot retrieval. _Proceedings of SPIRE 2022_.

#v(0.5em)
Fröbe, M., Reimer, J. H., MacAvaney, S., Deckers, N., Reich, S., Bevendorff, J., Stein, B., Hagen, M., & Potthast, M. (2023). The information retrieval experiment platform. _Proceedings of SIGIR 2023_. arXiv:2305.18932.

#v(0.5em)
Gutiérrez, B. J., Shu, Y., Qi, W., Zhou, S., & Su, Y. (2025). From RAG to memory: Non-parametric continual learning for large language models. _Proceedings of ICML 2025_. arXiv:2502.14802.

#v(0.5em)
Han, Z., Mankikar, M., Michael, J., & Wang, Z. (2025). Search-time data contamination. _arXiv preprint_ arXiv:2508.13180.

#v(0.5em)
Howard, M. W., & Kahana, M. J. (2002). A distributed representation of temporal context. _Journal of Mathematical Psychology, 46_(3), 269–299.

#v(0.5em)
Islam, M. A., Shuvo, M. N. U., Waseem, M., & Abrahamsson, P. (2026). LEDGER: Auditable agent memory under change. _arXiv preprint_ arXiv:2611.20927.

#v(0.5em)
Khan, M. N., & Kumar, K. S. S. (2026). Soei: Structured observability of evidence and intent for multi-agent AI software engineering. _arXiv preprint_ arXiv:2610.39468.

#v(0.5em)
Ko, A. J., DeLine, R., & Venolia, G. (2007). Information needs in collocated software development teams. _Proceedings of ICSE 2007_, 344–353.

#v(0.5em)
LaToza, T. D., & Myers, B. A. (2010). Hard-to-answer questions about code. _Proceedings of PLATEAU 2010_. ACM.

#v(0.5em)
Liu, Y., Fang, J., & Qian, C. (2026). When agent context goes stale: Incoherence in volatile agent context. _arXiv preprint_ arXiv:2610.05281.

#v(0.5em)
Naur, P. (1985). Programming as theory building. _Microprocessing and Microprogramming, 15_(5), 253–261.

#v(0.5em)
Rocchio, J. J. (1971). Relevance feedback in information retrieval. In G. Salton (Ed.), _The SMART Retrieval System: Experiments in Automatic Document Processing_ (pp. 313–323). Prentice-Hall.

#v(0.5em)
Sung, J., Zhou, K., Qian, G., Xiao, H., & Yi, L. (2026). Reliability stress testing of RAG retrieval under query-relevant corpus poisoning. _arXiv preprint_ arXiv:2611.04264.

#v(0.5em)
Xiao, S., Chen, X., Liu, H., Zhang, W., & Tang, Y. (2026). SWE-Time: Benchmarking temporal repository understanding. _arXiv preprint_ arXiv:2611.12083.

#v(0.5em)
Xu, H., Zhou, L., Jin, Z., Zhang, X., Tang, B., Li, Z., Zhou, X., & Zhang, J. (2026). MemTrace: State-consistent memory for long-horizon coding agents. _arXiv preprint_ arXiv:2610.04838.

#v(0.5em)
Xue, S., Zhang, Z., Liao, Z., Qin, J., Mu, Y., & Yu, H. (2026). Embeddings vs. grep: Rethinking the dense advantage in code search. _arXiv preprint_ arXiv:2611.20455.

#v(0.5em)
Zighelnic, L., & Kurland, O. (2008). Query-drift prevention for robust query expansion. _Proceedings of SIGIR 2008_, 825–826.
