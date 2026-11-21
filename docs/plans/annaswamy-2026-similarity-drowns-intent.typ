// Three-Trace Sagas — Typst source
// US Letter, single column, 11pt New Computer Modern, justified.

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

// ---------- Figure-drawing helpers (built-in primitives only) ----------
// Rounded box for flow diagrams, absolutely placed.
#let dbox(x, y, w, h, body, fs: 9pt) = place(
  dx: x, dy: y,
  box(width: w, height: h, radius: 4pt, stroke: 0.7pt, fill: luma(248),
    align(center + horizon, text(size: fs, body))),
)
// Plain vertical / horizontal connector lines.
#let vseg(x, y0, y1) = place(dx: x, dy: y0, line(angle: 90deg, length: y1 - y0, stroke: 0.7pt))
#let hseg(x0, x1, y) = place(dx: x0, dy: y, line(length: x1 - x0, stroke: 0.7pt))
// Downward arrow: line segment plus a small filled triangle whose tip is at y1.
#let varrow(x, y0, y1) = {
  vseg(x, y0, y1 - 4pt)
  place(dx: x - 3pt, dy: y1 - 5pt, polygon(fill: black, (0pt, 0pt), (6pt, 0pt), (3pt, 5pt)))
}
// Paired slope-graph helpers (line, rect, place, text only — no charting package).
// One A-to-B connector: a line segment plus small square markers at each endpoint.
#let sline(x0, y0, x1, y1, col, th) = place(line(start: (x0, y0), end: (x1, y1), stroke: th + col))
#let smark(x, y, col) = place(dx: x - 2pt, dy: y - 2pt, rect(width: 4pt, height: 4pt, fill: col, stroke: none))
#let sedge(xa, xb, ya, yb, col, th) = {
  sline(xa, ya, xb, yb, col, th)
  smark(xa, ya, col)
  smark(xb, yb, col)
}
// Small colored annotation label (used to mark line multiplicity / direction).
#let slabel(x, y, col, body) = place(dx: x, dy: y - 4pt, text(size: 7.5pt, fill: col, body))

// ---------- Title block ----------
#align(center)[
  #text(size: 17pt, weight: "bold")[Similarity Drowns Intent: Three-Trace Sagas and Reinstatement Recall for Provenance in Agentic Software Construction]

  #v(1.2em)
  #text(size: 12pt)[Rama Annaswamy] \
  #text(size: 11pt)[Independent Research]

  #v(0.6em)
  #text(size: 11pt)[November 21, 2026 · version 2]
]

#v(2em)

// ---------- Abstract ----------
#align(center)[#text(size: 12pt, weight: "bold")[Abstract]]
#v(0.6em)
#pad(left: 0.75in, right: 0.75in)[
  #set text(size: 10pt)
  #set par(justify: true)

  A coding agent leaves a record of its own work: prompts, deliberation, edits. We study _provenance_ questions over that record — "why does this code look like this?" — organized as _sagas_, joint traces of _intent_, _deliberation_ and _artifact_, on one operator's history, and we report what held up after the resulting system shipped. In that history, what an agent could recover from recorded work depended on what the system captured, which retrieval path a query activated, and what actually reached the agent; benchmark scores alone missed failures at each of those steps.

  #v(0.6em)

  _What held._ Flat cosine retrieval answers a why-question with restatements of the question. _Reinstatement recall_, an LLM-free seed-conditioned walk, improved ground-truth session coverage over one-shot cosine kNN by +53% at equal budget in a pre-registered spike (7 per-query wins, no losses), and leads on graded provenance gold (+37% origin-MRR, +29% `nDCG@10`, +25% graded recall: point estimates on 12 mapped queries, no intervals; the owner audited only the five injected-origin maps, and vendor splits and grade spot-checks went unaudited). Evaluating the system exposed _self-indexing evaluation contamination_: the system ingests the evaluation dialogue, and echoes of the questions displace the origin conversations being sought. In a controlled dose-response experiment five scripted re-ask cycles took about half of naive retrieval's top-10, query-echo defenses removed 72% of that occupancy, and a size-matched sham left kNN rankings unchanged. In a blinded benchmark of 16 sealed re-litigation tasks, the two recall arms scored 10 and 9 of 12 rubric points on settled-for-cause tasks against 6 of 12 for a no-recall arm, and the walk and kNN tied on verdicts (identical on all 16 tasks) and on the reason-quality total (27 of 36 each). A pre-registered ratification-weighting hypothesis halted at $rho approx 0$ across three extractors, and that null forbade inferring state from prose in the deployed system, which records asserted, user-confirmed verdicts instead.

  #v(0.6em)

  _What did not._ A pre-registered cross-source benchmark against a baseline with a lexical channel failed both gates. On 396 exact receipt-lookup queries hybrid kNN+FTS beats the walk at recall\@5, 0.813 to 0.581 ($p approx 6.9 times 10^(-20)$); on 27 sealed cross-source questions the arms do not separate (an underpowered null; a no-recall arm scores 0 of 27). A post-hoc ablation and code trace propose, as an untested diagnosis, that the echo defenses cause the reversal: of the 103 receipt queries the hybrid arm found and the walk missed, the trace assigns 63 to echo-aware seed selection and 40 to stacked demotions. If that holds, contamination defense and receipt lookup compete for one signal, and routing retrieval by query class is worth testing. Both gate outcomes replicated after a fourth contamination vector (hook-recursion self-transcripts: 84% of a frozen corpus's conversations and about 37% of its chunks) was found post-hoc and scrubbed. A sham-controlled ablation could not separate the co-edit channel's contribution from zero (+0.017 origin-MRR, 95% CI [0.000, +0.050]) and found an AST-structural arm indistinguishable from its own edge-shuffle sham. An advantage of the walk over retrieval with a lexical channel is therefore not supported, and the shipped system's walk and its hybrid, cross-encoded general recall have never been compared.

  #v(0.6em)

  _What shipping added._ Deterministic code-witness verdicts score precision 1.000 on a release-tag staleness benchmark, yet an audit of the verdict lines actually delivered to sessions under an earlier build judged 223 of 374 false, and in one snapshot at most 1,710 of 1,052,345 indexed chunks were bound to any witness. On the operator's own logs, context pushed at prompt time showed no more next-turn identifier overlap than a same-project control that was not shown (21 against 22 of 1,502); after a context compaction, detected late-needed detail came back mostly from the workspace, through host re-attachment and agent file reads, rather than from recall; and the frozen question bank contained none of the file- or symbol-naming queries that agents occasionally issue. These field results are single-operator observations whose receipts are local and unreleased, and most are nulls. The paper reports only results whose receipts are in the record. External and multi-operator validation remain open.
]

#v(0.6em)
#pad(left: 0.75in, right: 0.75in)[

#text(size: 10pt)[*Keywords:* provenance, coding-agent transcripts, reflection over recorded work, retrieval, evaluation contamination, temporal context model, MCP]
]

#v(1.5em)

// ==================== 1. Introduction ====================
= Introduction

In _Programming as Theory Building_, Naur argued that source code is not the primary artifact of software work. The real artifact is the _theory_ in programmers' heads — the web of intentions, constraints, and rejected alternatives that explains why the code has the shape it does. When those people leave or forget, the theory is lost, and later maintainers inherit a brittle surface that no longer answers "why" (Naur, 1985).

Agentic software construction sharpens Naur's problem. The "programmer" is no longer a stable human team but a transient human–agent collaboration stretched across many sessions, subagents, and tool traces. Intent is expressed in prompts; deliberation unfolds as reasoning and tool calls (including dead ends and sidechain chatter); artifact materializes as file-level edits. The theory that would justify a later design decision is distributed across those three traces and is _more_ perishable than classical human theory-building: sessions end, context windows compact, subagent transcripts fragment into separate files, and the human who held the intent may never re-open the originating conversation.

Most retrieval systems for coding agents treat the agent's recorded history as a flat bag of text: embed utterances, retrieve top-_k_ by cosine similarity, and hope the right chunk appears. For _lexical_ or _local_ questions ("where is `integrity_check` defined?"), this often works. For _provenance_ questions — "why is the integrity check cached?", "why was this API shape chosen?" — flat nearest-neighbor search fails on the corpora we study in a predictable way. The query is textually closest to agent restatements, tool-mechanic chatter, or later sessions that _discuss_ the decision, not to the human-origin conversation where the decision was made. Similarity drowns intent.

The failure is easy to exhibit on a live system. Asked "why did we drop Qdrant?" on the corpus studied here, one-shot kNN's top hit is the transcript of the user asking that same question earlier the same day; ranks two and three are further echoes of the question. The same query under reinstatement recall surfaces the migration analysis, the memory-limit incident preserved in a subagent transcript ("the Qdrant vector database had no memory limits set"), and the months-earlier session in which the replacement storage engine was actually built — a conversation that never contains the words "dropped Qdrant." A retrieval system that answers a why-question with a recording of the question has recalled nothing; the rest of this paper is about what to build instead, and about how easily measurement itself reproduces this failure mode.

This paper makes two measured contributions and one design contribution, in that order. Measured: (1) _reinstatement recall_, a seed-conditioned multi-hop retrieval algorithm for provenance questions, evaluated against one-shot cosine kNN under pre-registered gates and on graded gold, and then against hybrid retrieval in a pre-registered cross-source benchmark, which it lost; (2) _self-indexing evaluation contamination_, a reproducible failure mode of self-recording systems, with defenses that measurably repair it. Design: the _three-trace saga_ — intent, deliberation, artifact as an organizing structure for an agent's record of its own work. We are explicit about the boundary: mechanism attribution moved as the gold improved — raw channel counts against file-touch coverage credited the seed-conditioned semantic blend, and the controlled ablation against decision-graded gold credited code-graph spread as a within-grid ordering on partly unaudited grades — and a third movement arrived with interval estimates: a five-arm sham-controlled grid on the same sealed gold leaves the within-grid ranking standing while showing that the co-edit channel's marginal contribution is not separable from zero at 12 mapped-origin queries of 20, and that an AST-structural arm is indistinguishable from a degree-preserving edge-shuffle sham. Within-grid ranking survives; causal separation does not. We fix the term here once: throughout this paper _code-graph_ names the session↔file co-edit trace (`code_evolution`) and nothing else — the AST symbol graph (`code_nodes` / `code_edges`) was never read by the historical walk measured in the spike and on graded gold (the sham-controlled follow-up measured an explicit AST arm separately, as a null, and the shipped 10.1 walk reads the AST tables on symbol-naming queries, unmeasured; see System Design), and its measured contribution is provenance integrity, not provenance ranking. Source-factored use of the three traces (weighting intent vs. deliberation vs. artifact channels separately) is implemented in storage but not yet ablated; the saga is the architecture the results motivate, not one they isolate. We implement the system as Self-Reflect (`claude-self-reflect`, CSR): a single Rust binary with local MiniLM embeddings (384-dim, FastEmbed), SQLite storage and ast-grep code graph construction. At evaluation time (July–August 2026) it used HNSW vector search and exposed fourteen MCP tools and six Claude Code lifecycle hooks; the shipped 10.1.0 release (November 2026) is described in System Design and differs in ways this paper states explicitly. The provenance path is reinstatement recall (`csr_why`): seed with one-shot kNN, blend query and seed context for a second hop, spread activation over a session–file code graph, hop one step along episode chains, then fuse and cap.

We evaluate on dogfooded multi-hop questions about CSR's own development history, with pre-registered proceed/iterate/kill gates. The spike passes. The contamination result rests on a controlled experiment (E3) and on the T3 scrub: self-indexing systems exhibit an observer effect that evaluation design must confront.

*Scope of this paper.* The retrieval experiments were run on a research build in July and August 2026. This paper does three things. It reports every experiment whose receipts are in the record, including a pre-registered cross-source benchmark (T3) in which the walk _loses_ exact receipt lookup to hybrid retrieval and does not separate from it on multi-hop questions. It describes the system that shipped as 10.1.0 and marks, mechanism by mechanism, what is measured, what is built and unmeasured, and what the shipped code does differently from the measured code. And it reports field measurements taken on the operator's own corpus in October–November 2026, most of them nulls: context pushed at prompt time shows no more next-turn identifier overlap than a same-project control that was not shown; verdict lines that scored precision 1.000 on a benchmark population were mostly judged false on the delivered surface under an earlier build; the provenance tool this paper introduced is rarely called; and after a context compaction, detected late-needed detail comes back mostly from the workspace. These are single-operator observations with local, unreleased receipts, and we report them with the same prominence as the wins. The system is best described as an agent _reflecting on its own recorded work_ — recall, recap, staleness verdicts, recovery after compaction — with storage as plumbing; we do not position it against memory products, and for the system's function we say "recall over recorded history".

// ==================== 2. Related Work ====================
= Related Work

*Temporal context and reinstatement.* Howard and Kahana's Temporal Context Model (TCM) proposes that episodic recall is driven not only by item–item similarity but by reinstatement of a drifting context representation that bound items together at encoding (Howard & Kahana, 2002). Our context-blend step — forming $0.65 dot q + 0.35 dot s$ from query vector $q$ and seed vector $s$, renormalizing, and re-searching — is a deliberate computational analogue: retrieval reinhabits the contextual neighborhood of a retrieved memory rather than matching the literal cue alone. The Context Maintenance and Retrieval (CMR) model extends TCM with source and organizational structure (Polyn, Norman, & Kahana, 2009). CMR motivates treating intent, deliberation, and artifact as distinct but contextually bound sources; our Phase-1 system still fuses them into a single blend rather than source-factored reinstatement, which we flag as future work.

*Relevance feedback in information retrieval.* Our context-blend step has a clear IR ancestor: Rocchio's relevance feedback (Rocchio, 1971) and its pseudo-relevance variants expand a query toward the centroid of top-ranked documents and re-search — formally close to our $q' = "renorm"(0.65 q + 0.35 s)$. We claim no novelty for vector blending itself. The differences that matter here: PRF re-searches once from a _centroid_ of assumed-relevant documents to improve topical recall, while reinstatement walks _per-seed_ — each seed spawns its own blended re-query plus graph and episode expansions — and optimizes a different objective (origin-of-decision, not topical relevance). Recent evaluation work sharpens the contrast: Deng (2026), stress-testing agent-memory retrieval under entity collision with pinned BM25 floors and paired-bootstrap intervals, measures lexical PRF (RM3) as a _null_ on intent-style memory queries — "no PRF expansion over the lexical channel substitutes for a dense encoder on intent-style queries" — and finds it regresses some strata via query drift. That drift, PRF's canonical failure mode, reappears in self-indexing agent histories in an aggravated form: the corpus continuously ingests restatements of the very queries being asked, so drift is toward the system's own echoes. The query-echo defenses of Phase 1.5 (verbatim-echo demotion, echo-aware seed selection) are, in IR terms, drift countermeasures specialized to self-recording corpora, and we position them as a contribution to that older literature as much as to retrieval over agent histories. A dense per-seed centroid-PRF baseline under our shared reranker remains the fair head-to-head and is queued in the ablation grid.

*Rational analysis of memory access.* Anderson and Schooler's rational analysis argues that human memory's sensitivity to recency and frequency mirrors a Bayesian estimate of future need given environmental statistics (Anderson & Schooler, 1991). This supplies a theoretical warrant for _learned decay_: weighting retrieval by logged access patterns rather than fixed similarity alone. CSR previously logged retrieval only on the prompt-submit hook path; Phase 1 adds MCP-path logging with the aim of usage-weighted reinstatement once logs accumulate. As built in 10.1.0 that logging cannot feed a decay (Discussion, Silently-inert mechanisms).

*Programming as theory building, and the software-engineering lineage.* Naur's thesis frames provenance recall as the underlying problem: the goal is not merely to find similar text but to reconstitute the theory that produced the code (Naur, 1985). In agentic settings, that theory lives disproportionately in the _intent_ trace — human prompts and the human–agent dialogue that fixed constraints — while deliberation and artifact record how and what, respectively. Joint indexing without a path back to intent recreates theory loss at machine speed. The question is empirically grounded in the software-engineering literature: rationale ("why was it done this way?") ranks among the hardest information needs developers report (Ko, DeLine, & Venolia, 2007), and intent/rationale questions dominate developers' hardest-to-answer questions about code (LaToza & Myers, 2010). The classical responses to rationale capture — design rationale from IBIS onward (Kunz & Rittel, 1970; Moran & Carroll, 1996) and architecture decision records (Nygard, 2011) — require humans to _author or curate_ the rationale record; automated traceability recovery (Wu et al., 2011) links existing artifacts but captures no rationale. Agentic construction changes the economics: the rationale record now writes itself as a side effect of doing the work, and the open problem moves from capture to retrieval — which is where this paper sits.

*Retrieval over agent histories and graphs.* We position mechanisms, not products, and group prior work by mechanism. _Linking at write time, one global search at query time:_ A-MEM (Xu et al., 2025) builds Zettelkasten-style linked notes, but its links are constructed when a note is written and retrieval remains one global cosine kNN, with no expansion at query time. _Temporal validity of stored facts:_ Zep/Graphiti (Rasmussen et al., 2025) maintains a bi-temporal entity and fact graph over conversations and business data, searched by a hybrid of semantic, BM25 and traversal; it answers "what was true when" over entities rather than code symbols. _Graph expansion at query time:_ HippoRAG 2 (Gutiérrez et al., 2025) seeds nodes and runs Personalized PageRank over an OpenIE triple graph, on static QA corpora rather than self-recorded agent history. MAGMA (Jiang et al., 2026) traverses four graphs, including LLM-inferred _causal_ edges, by beam search from rank-fused anchors, with a router that up-weights causal edges for "why" queries; its causal graph is entailment over dialogue events (LoCoMo), not AST-anchored code artifacts, and its traversal is not per-seed reinstatement. _Defence against a system's own echoes:_ none of these guards against the retrieval surface being flooded by echoes of its own recorded queries, which is the failure mode this paper measures and defends against. _Evaluation method:_ Deng (2026) contributes methodology rather than a system (entity-collision stratification, pinned BM25 floors, bootstrap intervals) and is discussed under relevance feedback above; we adopt its discipline as the standard our queued ablation grid should meet.

*Concurrent 2026 preprints.* Three concurrent works occupy immediately adjacent space; the positioning below is based on close reading of the full texts. ACT-Up (Thomson & Lebiere, 2026) is not an agent-memory system but a cognitive-modeling toolkit: a working-memory and spreading-activation module for the ACT-Up architecture, combining base-level decay with spreading activation from working-memory context and validated against human serial-recall data (conditional response probability fits, $r^2 > 0.9$, on the Klein–Addis–Kahana paradigm). Its recall loop is genuinely iterative — "preliminary retrievals continue to be sources of activation, creating a robust chain of contextual information to prime subsequent recall" — which makes it mechanistic _grounding_ for our reinstatement walk rather than a competing system: it validates the recall dynamics we borrow, on human data, with no agent, corpus, or code anywhere in scope. MRMS (Li & Shi-Nash, 2026) is architecturally similar: a multi-resolution memory substrate whose typed memory graph carries provenance-relevant edges (_supports, contradicts, supersedes, derived-from_) and whose retrieval pipeline follows kNN with typed graph expansion. It differs on four axes: interaction traces are undifferentiated (no intent/deliberation/artifact taxonomy), there is no code or artifact awareness, its evidence-attribution task is single-hop claim-to-trace selection rather than multi-hop "why" chains, and its evaluation is 800 deterministically generated synthetic tasks with, by its own description, "no private data, production data, external API, or LLM judge" — a complementary regime to the production-corpus evaluation pursued here. E-mem (Wang et al., 2026) preserves _uncompressed_ episodic context — raw dialogue/document segments held by per-segment assistant agents — retrieved by multi-pathway activation with an iterative refine-and-query loop, and is the one system of the three with real multi-hop evaluation (LoCoMo, HotpotQA). But its stream is a single undifferentiated token sequence: no artifact or code trace, no human-intent typing, no code graph, and its multi-hop questions are conversational QA, not code provenance. Across all three: none jointly indexes intent+deliberation+artifact, none is code-graph-aware, and none evaluates provenance "why" questions on a production corpus. This paper's contribution is therefore joint three-trace indexing, a specified and measured reinstatement algorithm, and a methodological finding about observer effects in self-indexing systems — not a claim of exclusive novelty in retrieval over agent histories writ large. After T3 the reinstatement algorithm's standing is the narrower one stated in the Discussion: measured to beat cosine kNN on coverage, not measured to beat hybrid retrieval.

*Further related work.* An obvious ancestor is Hipikat (Čubranić et al., 2005), which built a project record from heterogeneous software artifacts. Search-time contamination of benchmarks is a neighbouring finding (Han et al., 2025). Recent work in the same space covers the following: on responses that collapse when retrieved documents are self-authored (Druck & Smith, 2026); on staleness of stored agent context (Concord, LEDGER and MemTrace); on attribution recorded at write time (Agent Trace, 2026); on context compaction (Chen, 2026; CompactionBench; CompAct-Bench); on evaluating whether retrieved context is used and whether completion claims are proven (Baklanov et al., 2026; Khan & Kumar, 2026); and on dense against lexical retrieval in code search (Xue et al., 2026). MemTrace (Xu et al., 2026) is the closest combined precedent for our witness verdicts and has what ours lacks, an end-task evaluation. Appendix H sets each of these against this paper's claims.

// ==================== 3. System Design ====================
= System Design

== Architecture overview

CSR is a single Rust binary (`csr-engine`) that replaces an earlier Python/Docker/Qdrant stack. This subsection first describes the build that was evaluated (July–August 2026), then the build that shipped (10.1.0, November 2026). Every retrieval number in the Evaluation and Phase 2 experiments sections is for the July–August 2026 research build, and each experiment states its own query set, build and date; the November recall-bank figures in Field measurements are on the shipped engine. At evaluation time the corpus comprised over 70,000 indexed chunks across roughly 530 conversations, together with stored reflections, an AST-derived code graph, and episode anchors; exact per-snapshot chunk counts appear in the Method section, where they document contamination states. Embeddings are local (MiniLM, 384 dimensions via FastEmbed); search uses HNSW (`hnsw_rs`); persistence is SQLite (`rusqlite`). AST analysis via ast-grep populates `code_nodes` / `code_edges`, linking conversations to files and functions they touched. A `code_evolution` table records the session↔file timeline used both for graph spreading and for ground-truth construction in evaluation. Three layers are distinct here and are never collapsed in what follows: (i) the AST symbol tables `code_nodes` / `code_edges`; (ii) the session↔file co-edit ledger `code_evolution`; (iii) symbol→conversation attribution. In the evaluated build only the co-edit ledger is read by the retrieval walk's graph-spread step (Step 3 below); the AST tables supply definition-versus-name-only labeling, line anchors, and visible abstention on the consumer surfaces; attribution is the layer a user consumes when asking who introduced a symbol, and is measured on its own terms rather than by retrieval metrics (Limitations, Deferred work). The evaluated build exposed fourteen MCP tools and six Claude Code hooks (SessionStart, UserPromptSubmit, PostToolUse, Stop, PreCompact, SessionEnd) that capture sessions continuously.

*As shipped (10.1.0).* The released system differs from the evaluated one in five ways that matter for reading this paper; all are in the public source tree, and none has been evaluated against the gold sets below unless stated. (1) _Surface._ Sixteen MCP tools are registered by default, plus one measurement tool behind an environment gate (`csr-engine/src/mcp/mod.rs`), and seven hook events are installed — the six above plus PostCompact (`csr-engine/src/hooks/install.rs`). (2) _General recall is hybrid and cross-encoded._ `reflect_on_past` — the tool whose one-shot kNN core is Arm A in this paper — now fuses three rankings (dense kNN, SQLite FTS5 BM25, and exact identifier postings) by weighted reciprocal-rank fusion (Cormack et al., 2009) and reranks a 24-candidate pool, plus session representatives, with a tuned 32M-parameter cross-encoder compiled into the binary (`search/hybrid.rs`, `search/identifiers.rs`, `search/cross_encoder.rs`). If the reranker cannot run, the call falls back to the fused order; how often that happens in use is not measured. The cross-encoder runs on explicit recall calls only: never in hooks, and not inside `csr_why`. (3) _The vector index is an immutable generation._ Chunk kNN is served from a USearch f16 index built as a hash-stamped, never-mutated generation by the background daemon (`index_gen/`); stored reflections remain on `hnsw_rs`. (4) _The corpus is multi-source._ Besides main-session transcripts, the importer ingests subagent transcripts (attributed to their parent session), plan files, a second vendor's agent rollouts, and the host's prompt-history file as a session registry that is never embedded; fixed precedence rules (origin conversation beats plan copy; parent beats subagent copy) are applied before the page cut. (5) _Scale._ On 2026-11-16 the operator's store held 1,052,345 chunks (receipt R-temporal, Evidence register).

== Saga schema (Phase 1 data model) <sec-saga>

A _saga_ is the joint structure over one unit of work (the term is unrelated to the distributed-transactions Saga pattern; we use it in the narrative sense — the connected story of how a piece of code came to be):

+ *Intent* — human prompts and human-authored dialogue that state goals and constraints.
+ *Deliberation* — agent reasoning, tool-call traces, dead ends, and sidechain/subagent chatter.
+ *Artifact* — resulting code changes, tracked file-by-file and session-by-session in the code graph.

Phase 1 production changes make saga structure explicit in storage:

- *`chunks.seq`* — import order within a conversation (aligned with the existing chunk index that feeds UUIDv5 chunk ids). Enables future chunk-level temporal reinstatement; not yet used in scoring.
- *`chunks.is_sidechain`* — true if any source message has the JSONL `isSidechain` flag _or_ the conversation id starts with `agent-`. Over-labeling was deliberate: live Claude Code places subagent transcripts in separate `agent-*.jsonl` files, so inline `isSidechain` flags were effectively absent. The `agent-` prefix rule labeled 10,907 of 33,015 backfilled chunks (a count recorded in the operator's phase specification, not in the repository). Labeling is present; sidechain-aware rerank weights are deferred.
- *`retrieval_events` for MCP* — MCP-path searches are logged with `hook_phase='mcp_search'` and sentinel `session_id='mcp'`. Previously only the prompt-submit hook path logged retrieval, rendering the dominant MCP recall path invisible to any future frequency/recency mechanism. Logging was meant to unblock learned decay; it does not implement it, and as built it cannot feed one (Discussion, Silently-inert mechanisms).

Deferred non-goals at Phase 1 (explicitly not claimed as done then): learned decay weighting; chunk-level temporal reinstatement via `seq`; SessionStart saga narration UX; sidechain-aware scoring weights.

*Status of these in 10.1.0.* Sidechains are now imported by path with a parent conversation and deduplicated under it, still with no score weight. A SessionStart recap shipped as a deterministic, zero-LLM composition with receipts attached, framed to the agent as history rather than instructions; a clause with no evidence behind it is dropped. Usage-weighted decay is built and structurally inert (Discussion, Silently-inert mechanisms). Chunk-sequence reinstatement remains unbuilt. Appendix C gives the detail and maps the three traces, and a fourth role, _settlement_, onto the shipped tables.

== Reinstatement recall (`csr_why`)

The algorithm as evaluated implements five steps. Defaults: $k = 10$ (max 50), seeds $= 3$, `blend_query_weight=0.65`, `graph_boost=1.10`, `graph_cap_per_seed=6`, `min_score=0.20`. No LLM participates in the retrieval path — only vector and graph arithmetic. No latency figure is reported for the walk: no timing receipt exists for the evaluated build, and the 10.1.0 walk, which does more work (below), has not been timed.

*Step 1 — Seed retrieval.* Compute top-3 nearest neighbors over the query against chunks and stored reflections merged by cosine similarity. At evaluation time this was exactly the one-shot baseline used by `reflect_on_past` (minus presentation formatting); it no longer is (Architecture overview).

*Step 2 — Context blend (second hop).* For each seed with vector $s$, form a blended query

$ q' = "renorm"(0.65 q + 0.35 s) $

and re-run kNN with $q'$. This is the TCM-style reinstatement step: search from the context associated with the seed memory, not only from the literal cue.

*Step 3 — Code-graph spreading activation.* From each seed's session, look up files touched in `code_evolution`, find other sessions that touched the same files, and pull the best-matching chunk (cosine vs. the _original_ query) from each neighbor session, with a small activation boost (`graph_boost=1.10`). Cap at `graph_cap_per_seed=6` to limit over-spreading.

*Step 4 — Episode chain hop.* From the seed session's episode reflection, follow one step of `prev_episode_id` (chronological chain of session summaries) and pull the immediately prior episode when present.

*Step 5 — Fuse, rerank, dedupe, cap.* Merge channels, deduplicate, apply `min_score`, rerank the fused pool under a provenance-aware policy (Phase 1.5, below), and return at most $k$ results as grouped evidence chains citing conversation ids.

#[
#set figure.caption(position: bottom)
#figure(
  kind: image,
  supplement: [Figure],
  block(width: 100%, height: 184pt, {
    // query box
    dbox(181pt, 0pt, 70pt, 20pt, [query $q$])
    varrow(216pt, 20pt, 38pt)
    // Step 1
    dbox(131pt, 38pt, 170pt, 20pt, [Step 1: kNN seeds (top-3)])
    // fan-out bus to three channels
    vseg(216pt, 58pt, 70pt)
    hseg(72pt, 360pt, 70pt)
    varrow(72pt, 70pt, 84pt)
    varrow(216pt, 70pt, 84pt)
    varrow(360pt, 70pt, 84pt)
    // three parallel channel boxes
    dbox(4pt, 84pt, 136pt, 48pt, [Step 2: context blend \ $q' = "renorm"(0.65q + 0.35s)$], fs: 8pt)
    dbox(148pt, 84pt, 136pt, 48pt, [Step 3: code-graph spread \ (files $arrow.r$ sessions)], fs: 8pt)
    dbox(292pt, 84pt, 136pt, 48pt, [Step 4: episode chain hop \ (prev episode)], fs: 8pt)
    // fan-in bus to fusion
    vseg(72pt, 132pt, 144pt)
    vseg(216pt, 132pt, 144pt)
    vseg(360pt, 132pt, 144pt)
    hseg(72pt, 360pt, 144pt)
    varrow(216pt, 144pt, 158pt)
    // Step 5
    dbox(66pt, 158pt, 300pt, 22pt, [Step 5: fuse · dedupe · provenance rerank · cap $k$])
  }),
  caption: [Reinstatement recall as evaluated. One-shot kNN provides seeds only; three reinstatement channels re-expand from seed context before provenance-aware fusion.],
)
]

*The shipped walk (10.1.0) differs from the measured one.* The defaults above are unchanged in code (`csr-engine/src/search/reinstatement.rs`), but six behaviours were added after the evaluations in this paper, and none has been measured against the graded gold; Appendix C lists them. The one that matters most: when a query names an identifier that resolves in the AST tables, the walk _skips_ co-edit spread on that match alone, and launches structural seeds only where the symbol's transcript attribution or incident edges name a usable conversation; a match with neither launches no structural seed. Our ablations give this default no support: within its grid the channel ablation ranks co-edit spread first (with a marginal contribution that is not separable from zero), and the sham-controlled follow-up measured an AST arm as a null, on an older attribution path than the one the shipped route uses (Phase 2 experiments). The shipped route for symbol-naming queries is therefore untested rather than refuted, and the AST rematch listed under Deferred work is the test of the current route. `csr_why` does not consult witness verdicts or the resolution ledger; only general recall does.

== Provenance-aware rerank (Phase 1.5)

The fused pool is wide (`min_score=0.20`), so rank order inside it matters. Phase 1.5 applies CSR's existing rerank machinery to the pool under a dedicated `Provenance` policy, tuned by frozen-corpus evidence rather than by porting `reflect_on_past`'s weights wholesale. Two planned transfers were _rejected_ by per-query evidence: demoting tool-mechanic chunks evicted a ground-truth session (edit-heavy chunks are provenance _evidence_, not noise, in this task), and a flat user-authority boost promoted weakly relevant user chunks and user-role compaction summaries over strong evidence. Two defenses were added instead: (1) a *query-echo penalty* (−0.35) on chunks quoting the query near-verbatim — a session that _asks_ a question is not the session that _answered_ it — paired with echo-aware seed selection (2× seed over-fetch at evaluation time, wider in 10.1.0; non-echo hits preferred as walk seeds), and (2) classifying compaction summaries ("This session is being continued from a previous conversation…") as scaffold text. Without echo-aware seeding, a re-asked question drowns the walk in its own prior askings: in a live test recorded at the time (no committed receipt), the reinstatement pool for a repeated query collapsed to a few self-referential conversations, and with echo-aware seeding more conversations surfaced, with the answer-bearing chunk in the top group. The shipped policy carries further terms that the evaluations here did not isolate: a scaffold penalty (−0.30), a penalty on low-trust rows that assert a decision (−0.60), a supersedes boost (+0.20), and a primacy bonus (+0.15) within a narrow score band (`csr-engine/src/search/rerank.rs`). The T3 benchmark below suggests a cost side of the echo and scaffold demotions: a post-hoc trace, not tested by an intervention, proposes that on exact receipt lookups they demote the answer.

*Implementation note on scoring.* Per-conversation scoring uses exact cosine over that conversation's stored chunk embeddings (conversations average \~136 chunks — microseconds), _not_ a filtered HNSW search. Tiny allowed-id-set filters pathologically escalate `hnsw_rs` toward near-full-index scans; exact scan over a single conversation is both simpler and faster.

== Baseline for comparison

Arm A (baseline) is one-shot kNN over chunks+reflections — the `reflect_on_past` retrieval core at evaluation time. It has no lexical channel. The shipped `reflect_on_past` is hybrid and cross-encoded (Architecture overview) and was not an arm in any experiment. T3's K arm is a separate, historical baseline (dense plus full-text fusion, without identifier postings or the cross-encoder), and no experiment compares the walk with the shipped recall path. Arm B is the five-step walk above. Both share the same embedding space and storage. A fairness caveat we state plainly: after Phase 1.5, Arm B also carries the provenance reranker and echo-aware seeding while Arm A does not, so post-1.5 comparisons validate the full `csr_why` stack rather than the walk in isolation; only the Phase 0 spike isolates the walk. A factored comparison grid — each arm sharing the identical reranker, dedup, and budget, varying only candidate generation — has since been run in reduced form: five arms isolating the expansion channels (no expansion, co-edit spread, AST spread, degree-preserving AST sham, both spreads), with paired-bootstrap intervals, reported in @sec-e1b and released at `csr-engine/eval-kit/h1/`. The echo-demotion and centroid-PRF arms of the original specification remain unrun.

== Evidence-grounded forgetting (witness verdicts)

The asserted-state ledger established one boundary: retrieval may replay a cited verdict, but it should not infer state from plausible prose. Witness verdicts extend that boundary from asserted settlement to repository-verifiable evidence. On a background cadence the system asks a narrower question: does the code span a recorded claim was bound to still exist in the evidenced form? No model writes or judges a witness verdict.

_A note on the name._ The module that implements this mechanism is named `dream` (`csr-engine/src/dream/`). This paper says _deterministic witness verdicts_. In the shipped product "dreams" names something else: model-authored what-if suggestions in the companion pane, which are not evaluated here. The environment switch `CSR_NO_DREAMING` turns off the witness-verdict pass; it is not the switch for those suggestions. Everything in this section, and the T4 benchmark, concerns the deterministic verdict join only.

_Witness ledger._ `witness_ledger` is the durable substrate. At a commit OID it records the code anchor, a versioned `b3:` BLAKE3 stamp of the anchored span, and the identity needed to join the witness back to a claim or chunk. The ledger is append-only: later observations do not rewrite earlier witnesses; obsolescence and reinstatement are additional facts, so an exact revert restores a prior witness without erasing the interval in which it was absent. The unit is the span rather than the file — the v10 spike found file-level stamps report false drift for unchanged functions whenever neighbouring code changes. Commit OIDs order committed evidence; wall-clock time does not. Dirty-worktree observations are provisional and never become committed ledger facts. Seven repositories had been ledgered at evaluation time (by 2026-11-16 the operator's ledger held 624,542 rows over 1,952 files in 43 projects; receipt R-temporal); on the integrity gates used for this work, the August run passed its pre-registered bars with zero drifted edges. The exact closure and binding rates of that sealed run are omitted here because its receipt is not in the repository; the code-graph analysis note of 2026-07-31 in `docs/analysis/` records a live run at 98.0% closure and 85.4% binding. These are substrate measurements, not retrieval-effect estimates.

_Deterministic verdicts._ Verdicts are computed by witness closure and hash-equality joins, abstention-first: (1) resolve the claim to one ledger identity, else abstain; (2) establish the committed witness's relation to the evaluated commit by ancestry or content identity, else abstain; (3) span-hash equality in the same hash domain yields `Intact`; (4) absence yields `Vanished` only when the extractor can establish absence without a name collision; a changed span yields `Drifted`, a later evidenced replacement `Superseded{by, receipt}`; (5) a later hash-equal witness may reinstate an earlier belief, while ambiguous renames and A$arrow$B$arrow$A′ histories remain abstentions. No LLM anywhere in the verdict path, and no semantic claim that changed code is _wrong_ — only that an earlier code-bound statement no longer has the same witness. The public `codewitness` crate enforces these evidence tiers in its types: a superseded verdict requires a successor verified against a committed object (the original may be a worktree witness), and every receipt states its basis — graph-ordered, which establishes ancestry, or content-only, which depends on the caller's assertion that one span replaced another. Neither basis says who wrote the code or why it changed.

_Consumption: demote and annotate._ *Demote* partitions evidence before pagination, so a proved-obsolete item falls behind intact or unclassified evidence — it does not delete the item, stack a second decay penalty, or remove the receipt. *Annotate* leaves the evidence visible and attaches its verdict, witness identity, and commit receipt; live search renders these as `[evolved]`/`[stale anchor]` annotations carrying the supporting commit. The channels separate ranking policy from the evidential record: demotion can be disabled while annotations remain inspectable. The daemon revisits eligible work on a 6-hour cadence, plus an idle-gated nightly pass, and a kill switch disables the verdict pass without disabling search. The design called for fair (not newest-first) queue selection so that an accumulating import backlog cannot starve older witnesses — a failure already observed in the ratification pipeline; what we could verify in the shipped code is a shared queue between the heavy background passes, not an oldest-first witness selector.

_Reach._ A verdict can change what search shows only through a binding from a witness to a chunk. In one snapshot (2026-11-16) the operator's store held 160,067 verdicts and at most 1,710 of 1,052,345 chunks were bound (receipt R-temporal). The ledger records content at a commit and carries no session identifier, so the binding cannot be widened by joining on the session that wrote the code: in a second snapshot a heuristic file-path-plus-time-window join attached witness evidence to 853 of 648,712 conversation and subagent chunks (receipt R-temporal2). These are binding shares in two snapshots, not the share of real searches a verdict affects, which is unmeasured. They do say that the substrate gates above and the T4 precision below describe a mechanism that today can touch only a small part of what is retrieved. A session-keyed table of span hashes also exists, but it hashes different inputs from the witness ledger, so the two cannot be joined directly, and the coverage and wrong-link rate of any bridge between them are unmeasured.

_Release-ancestry decay is not an active consumer._ A decay keyed to release ancestry was designed as a second evidence-grounded consumer. In 10.1.0 it is built but off on the default recall path, and we do not claim it (Appendix C).

== What else ships around the walk (10.1.0)

The walk is one tool among sixteen. Five further mechanisms shipped in 10.1.0 and bear on the claims of this paper. Each is _built, not evaluated_ unless a receipt is named, and Appendix C describes each from the public source: guards against indexing the system's own output (structural cuts at import, label-only handling of text that merely matches the system's output grammar, a status counter per layer); a trust floor carried from the channel history; a secret-scrubbed transcript mirror, through which one host-deleted session was read back by the command-line path (on 2026-11-05, 19,737 of 20,935 mirrored files had no source left; recovery through the MCP tool is unproven; receipt R-compact-history); thread candidates derived from successive compaction summaries, with a quote-checked, reversible "guessed done" state whose predicate establishes only that a quote is present; and two-channel symbol attribution, which was once listed as future work and on which the AST rematch has still not been run. Appendix C also maps the three traces, and a fourth role, settlement, onto the shipped tables, and lists the six ways the shipped walk differs from the measured one.

// ==================== 4. Evaluation ====================
= Evaluation

== Method

We evaluate _provenance recall_: multi-hop questions of the form "why does the code look like this?", each paired with a ground-truth (GT) target file. GT is the set of sessions in `code_evolution` that historically touched that file. Primary metric *M1* is summed GT-session coverage across queries at equal budget $k = 10$. *M2* is distinct-conversation diversity in the top-10 (to check that gains are not mere result spraying). *M3* is qualitative side-by-side judging (better / tie / worse per query) plus a junk-rate estimate for B-only results, by a single annotator. These metrics and the file-touch GT belong to the Phase 0 spike; each later experiment states its own query set, gold, build and date with its results.

*Pre-registered gates* (set before running):

#figure(
  table(
    columns: (1fr, 4fr),
    stroke: none,
    align: (left, left),
    table.hline(),
    table.header([*Gate*], [*Criterion*]),
    table.hline(),
    [*PROCEED*], [B ≥ A + 25% summed GT coverage *and* B better on ≥50% of queries *and* junk rate under 30%],
    [*ITERATE*], [Mixed results; allow one weight retune and rerun],
    [*KILL*], [B ≤ A; write up honestly, thesis dies cheaply],
    table.hline(),
  ),
  caption: [Pre-registered gates.],
)

*Disclosed circularity.* Arm B's graph-spread step queries the same `code_evolution` table used to define GT, but only via seed sessions found by search: a bad seed yields no graph gain. The design is therefore not circular by construction, but shared plumbing could still inflate graph-channel hits. We therefore require mechanism attribution and M3, not M1 alone.

*Attempted independent GT from version control.* To break the shared-plumbing dependency entirely, we attempted a second GT construction from VCS metadata: commits in the project repository carry `Claude-Session` trailers naming the web session that produced them, giving a transcript-independent session-to-file mapping. The attempt failed instructively, in two ways. First, coverage: trailers existed on only a small fraction of commits when the evaluation was run (the count is omitted because it cannot be reproduced from the public history), all within days of writing — months-old decisions targeted by the eval queries predate the convention entirely. Second, and more telling, the bridge from trailer IDs to conversation IDs (locating the trailer text inside indexed transcripts) was itself contaminated by the observer effect this paper documents: the analysis session that ran `git log` to extract trailers thereby imported those trailer IDs into its own transcript, producing spurious mappings to the measuring session. We therefore report no independent-GT agreement number; the honest yield is a protocol recommendation — projects intending to evaluate provenance recall should emit session-to-commit trailers from day one, and should extract them outside the indexed environment.

*Corpus and query sets.* Live CSR development history (dogfooding). Each experiment has its own query set, build and date: the Phase 0 spike, 12 hand-written queries on the morning of 2026-07-15, run by a throwaway read-only binary against a 71,762-chunk DB; E2, 20 pre-registered queries (12 on this system's history, 8 on a second project of the same operator), 12 of them with a mapped origin, scored on rank lists frozen on 2026-07-15; E1 (2026-07-17), the same 20 queries on its own index build of the frozen snapshot; E1b, the same sealed gold on a frozen evaluation unit; E3 (2026-07-17), 8 queries on a session-zero snapshot of 17,134 chunks; the re-litigation benchmark (2026-07-20), 16 sealed tasks; T4, 13,626 release-tag beliefs at `5d2bd81`; and T3 (2026-07-27), 396 receipt-lookup queries and 27 sealed questions on its own frozen snapshot. The snapshot states used by the spike, E2 and E1:

#figure(
  table(
    columns: (1.6fr, 0.9fr, 1.1fr, 1.8fr, 1.6fr),
    stroke: none,
    align: (left, center, center, left, left),
    table.hline(),
    table.header([*Snapshot*], [*Chunks*], [*Queries*], [*Contamination state*], [*Index build*]),
    table.hline(),
    [Live, morning (spike)], [71,762], [12 (primary)], [pre-eval-dialogue (cleanest)], [fresh per run],
    [Frozen `eval-frozen-2026-07-15`], [73,342], [20 (graded-gold set)], [partial (includes spike-discussion session)], [one shared build per A/B comparison],
    table.hline(),
  ),
  caption: [Evaluation snapshot states.],
)

*Statistical treatment.* For the spike's 12 queries we report per-query outcomes and an exact paired sign test on wins/losses (ties excluded) rather than interval estimates: 7 wins / 0 losses (two-sided $p approx 0.016$). Summed coverage weights queries with larger GT sets more heavily; per-query win/tie/loss counts are the macro-level check. Graded origin-level metrics (origin-MRR, nDCG) arrived with the graded gold of E2. E2 and E1 report point estimates without intervals; E1b gives paired-bootstrap intervals for the expansion-channel contrasts only; T3 uses exact sign tests over discordant pairs.

== Phase 0 spike results

*Gate: PASSED.*

#figure(
  table(
    columns: (2.2fr, 1fr, 1.3fr),
    stroke: none,
    align: (left, center, center),
    table.hline(),
    table.header([*Metric*], [*Arm A (kNN)*], [*Arm B (reinstatement)*]),
    table.hline(),
    [Summed GT coverage ($k=10$)], [15], [23],
    [Relative lift], [—], [*+53%* (gate +25%)],
    [Per-query outcomes], [—], [B better 7/12, tie 5/12, worse 0/12],
    [Diversity (distinct conv. in top-10, summed)], [75], [77],
    table.hline(),
  ),
  caption: [Phase 0 spike results.],
)

Mechanism attribution of B's GT-hit result lines:

#figure(
  table(
    columns: (2fr, 1fr),
    stroke: none,
    align: (left, center),
    table.hline(),
    table.header([*Channel*], [*GT hits*]),
    table.hline(),
    [Context blend], [24],
    [Graph spread], [9],
    [Episode chain], [0],
    table.hline(),
  ),
  caption: [Mechanism attribution.],
)

In this count the majority of the win is from the pure-semantics blend hop, not the potentially shared-plumbing graph walk; at the time this was the main quantitative evidence against circularity as the sole explanation of M1. E1 later reversed the attribution on graded gold, so the argument no longer carries that weight.

*Qualitative highlight (Q3: "why is the integrity check cached").* Arm A's entire top-10 was subagent/tool-mechanic chunks with *zero* GT sessions represented. Arm B's blend hop surfaced the human-origin conversation (`7eccb720`) where caching was decided and discussed with the user — the failure mode the thesis predicts: one-shot similarity drowns in deliberation chatter while reinstatement digs back toward intent. B-only junk rate was eyeballed under 20%, mostly looser-but-relevant graph pulls rather than noise.

== Scope of the reported results

This paper reports the experiments whose per-run receipts are in the record. Other runs made during development are not reported, and no conclusion below rests on them.

// ==================== 5. Phase 2 experiments ====================
= Phase 2 experiments: graded gold, channel ablation, controlled contamination

Three experiments that were queued as the core follow-ups have been executed. Together they (i) replace the file-touch GT proxy with graded provenance gold, (ii) rank the walk's channels within one ablation grid, and (iii) convert self-indexing evaluation contamination from a documented incident into a dose-response phenomenon with a quantified repair.

== Graded provenance gold from ratification behavior (E2)

_Gold construction._ Rather than retrospective annotation, grades were derived from the operator's own contemporaneous dialog-acts and ratification behavior, under a protocol frozen before any retrieval ranks were seen. The operator first answered a sealed, pure-memory elicitation for all 20 queries — committed to version control _before_ any cueing or rank exposure — yielding strata: 12 origin descriptions recallable from memory, 7 unresolved (excluded from origin-ranked metrics, never soft-matched), and 1 out-of-corpus. Descriptions were then mapped to conversation IDs by metadata only (literal text search over pre-freeze chunks, git dates, external ship events — no embedding retrieval, which would reintroduce circularity). Candidate pools (both arms' frozen top-10s $union$ date-filtered file-touch history; 211 items, 205 reconstructible) were labeled by two independent LLM extractors from different vendors (Grok 4.5 and Claude Sonnet) applying a quote-anchored extractive protocol for DIRECTS / ACCEPTS / REJECTS / RE-ASKS acts; grading used strict two-vendor consensus, conservative on splits (directs agreement 84.8%, Cohen $kappa = 0.41$; splits were not audited: the owner audited only the five injected-origin maps; the 31 vendor-split items and the 12 grade spot-checks went unaudited). On accepts, the two extractors agreed on 98.5% of 204 items with Cohen $kappa = 0.395$ (the 0.41 above is for directs). About 5% of one vendor's quotes were not byte-exact verbatim (turn-boundary joins across timestamp markers and stripped markdown bold; content faithful, no fabrication observed); this is that vendor's own self-check, not an independent audit. Grade 3 was reserved for sealed-and-mapped origins — extraction can corroborate but never mint an origin. Pool construction and grade assignment have different dependencies: which items could be graded depends in part on the date-filtered file-touch history, which comes from the same `code_evolution` table the graph channel reads, while the grade an item receives comes from dialog-acts, sealed origins and ship ledgers. External ratification ledgers (git history across four repositories, npm publish timestamps, a release-train manifest) provide off-corpus acceptance evidence: across 204 extracted conversation-items the operator's dialogue contained 21 DIRECTS but only *1 explicit ACCEPTS* — operators direct in words and ratify by shipping, so independently timestamped ship events are the only acceptance signal that exists at scale.

_Results (12 mapped-origin queries; descriptive point estimates, no intervals)._ Reinstatement leads kNN on every graded metric: origin-MRR 0.264 vs. 0.193 (+37%), `nDCG@10` 0.470 vs. 0.363 (+29%), Recall\@10 of grade-$gt.eq$2 items 0.553 vs. 0.444 (+25%) — direction consistent with the coverage results, now on decision-graded gold. The sobering companion finding: both arms miss a large share of the verified origins. The receipt gives two counts for joint misses without a definition that separates them, so no figure is printed. The owner audited the five injected-origin maps and confirmed all five correct (map-error reading eliminated for those five); four of the five date from the corpus's earliest era — old, sparse origin conversations losing to later, denser sessions. Five is an audit count, not a joint-miss count. The 31 vendor-split items and the 12 grade spot-checks went unaudited, so these lifts rest on partly unaudited grades. Origin-finding is improved by reinstatement but not solved; the headroom above the winning system is large.

== Channel ablation (E1)

A seven-arm ablation (research harness mirroring the July 2026 walk, which is not the 10.1.0 walk; one process, one shared index build over 73,342 chunks; scored against the E2 gold; conversations outside the graded pool scored zero, a bias _against_ exploratory channels) decomposes the walk:

#figure(
  table(
    columns: (auto, auto, auto, auto),
    align: (left, right, right, right),
    stroke: 0.5pt + rgb("#cccccc"),
    inset: 5pt,
    [*Arm*], [*origin-MRR*], [*nDCG\@10*], [*R$gt.eq$2\@10*],
    [kNN baseline], [0.243], [0.394], [0.469],
    [full reinstatement], [0.276], [0.477], [0.559],
    [blend-only], [0.269], [0.446], [0.499],
    [*graph-only*], [*0.329*], [*0.555*], [*0.607*],
    [episode-only], [0.321], [0.457], [0.470],
    [full $minus$ rerank], [0.257], [0.447], [0.563],
    [full $minus$ echo-defense], [0.234], [0.434], [0.549],
  ),
  caption: [Channel ablation on graded gold, one shared index build. Within-grid comparisons only; absolute values are not comparable to the E2 results paragraph (different index build and harness).],
)

(In-text bridge for readers cross-checking tables: the kNN baseline row here, 0.243 origin-MRR, differs from the 0.193 reported in the E2 comparison because the grid ran on a separate index build (2026-07-17 against 2026-07-15) with a different harness; ANN construction is non-deterministic across rebuilds, so only within-grid comparisons are meaningful — the same discipline the contamination section motivates.)

Five findings. (1) _Graph-only ranks first within the grid_: the co-edit graph-only arm scores above the historical full walk on every metric (origin-MRR 0.329 against 0.276) despite the scoring bias against it. This reverses the spike-era attribution to the blend, which was counted against file-touch _coverage_ gold. The graded gold narrows the shared-plumbing concern but does not remove it: grades come from dialog-acts, sealed origins and ship ledgers, while the pool that was graded includes file-touch history from the same `code_evolution` table the graph channel reads (E2). Finding (1) rests on partly unaudited grades, and the arm ordering is unbracketed: the sham-controlled follow-up below measures co-edit spread against no expansion (`S+F` against `S`) at +0.0167 origin-MRR with a 95% interval whose lower bound is exactly zero, so finding (1) is a within-grid ranking and not a separated causal contribution. (2) _Fusion dilutes_: max-score fusion lets blend-sourced semantic neighbors crowd graph-sourced provenance neighbors out of the final budget; the tuning direction is a larger graph share, not more channels. (3) _Removing echo defense lowers the full arm_: origin-MRR falls from 0.276 to 0.234, below the grid's kNN baseline (0.243) — a within-grid point estimate on the same partly unaudited grades, without an interval. (4) _The episode chain, previously a null result, is a strong origin-finder on graded gold_ (0.321 origin-MRR) though weak on evidence depth — it finds the origin thread, not the full evidence set. (5) _The structural AST graph is not the channel that ranks_: the "graph" arm above reads only the co-edit ledger, and a follow-up (E1b) that adds an explicit AST-expansion arm finds it identical to the base arm on origin-MRR for all 12 mapped-origin queries and indistinguishable from its own degree-preserving sham; the AST subsystem's measured contribution is provenance integrity, not provenance ranking (next subsection).

== Sham-controlled channel follow-up (E1b) <sec-e1b>

The E1 grid reports point estimates, and its graph arm reads only the co-edit layer (`code_evolution`), so it says nothing about the AST symbol graph. A follow-up re-ran the expansion channels alone against the same sealed E2 gold: one process, one deterministic index build over a frozen evaluation unit, exact vector scan, identical seeds, rerank policy, quotas, and budget across arms, with the expansion channel as the only variable. Five arms: `S` (no expansion), `S+F` (session↔file co-edit spread — the channel E1 calls graph spread), `S+A` (AST symbol-graph spread over resolved `code_edges` only), `S+A-sham`, and `S+F+A`. The sham arm is the load-bearing control: it permutes edge destinations under a fixed-seed xorshift64 shuffle that preserves every node's degree, retaining the AST graph's density and shape while destroying its structural correspondence. An arm that cannot beat its own sham has not shown that structure, as opposed to edge count, is doing the work. Intervals are 10,000-resample paired bootstraps over per-query deltas (seed 20260731); origin-MRR is defined only on the 12 queries carrying a sealed mapped origin, `nDCG@10` over all 20. A methods note on determinism: after these numbers were sealed, a replication attempt on a second project of the same operator (quarantined, below) exposed hash-map iteration order in the harness's candidate pools as a per-process tie-break; the harness was made bit-deterministic (total order: score descending, id ascending), and re-measurement showed every origin-MRR contrast above bit-stable, `nDCG@10` drift $lt.eq$ +0.0044, and no verdict change — the sealed numbers are kept as the record of the frozen run. The kit is released at `csr-engine/eval-kit/h1/`; the frozen unit's sha256 is

#text(size: 9pt)[`a43fada5cd5f2954563707f75676521847005d9461f876b80e7082a8ae66f114`]

#figure(
  table(
    columns: (auto, auto, auto),
    align: (left, left, left),
    stroke: 0.5pt + rgb("#cccccc"),
    inset: 5pt,
    [*Contrast*], [*$Delta$ origin-MRR (12 mapped)*], [*$Delta$ `nDCG@10` (20 queries)*],
    [`S+F` $minus$ `S`], [+0.0167, CI [0.0000, +0.0500]], [+0.0264, CI includes 0],
    [`S+A` $minus$ `S`], [0.0000, identical on 12/12], [CI includes 0],
    [`S+A` $minus$ `S+A-sham`], [$approx$ 0], [+0.0002, CI [$minus$0.0132, +0.0139]],
  ),
  caption: [Sham-controlled expansion-channel contrasts on the sealed E2 gold, one shared index build, 10,000-resample paired bootstrap (seed 20260731). No contrast excludes zero.],
)

Neither graph channel separates from zero. The co-edit channel is directionally positive on both metrics and its origin-MRR interval touches zero exactly at the lower bound — consistent with E1's ordering, insufficient to certify it. The AST channel is a strict null: it returns the base arm's origin-MRR on every mapped query, and its `nDCG@10` difference from a graph with identical degrees and scrambled structure is +0.0002. The combined `S+F+A` arm is reported in the released kit and adds no separable gain over `S+F`.

Two bounds constrain how far the AST null generalizes, and we state both rather than banking the negative result. First, _sparsity_: the AST channel fired on 8 of 20 queries at all, over 4,092 resolved edges reaching 122 distinct conversation keys; a channel silent on most of the suite cannot be strongly falsified by it. Second, and more consequential, _attribution quality_: the arm maps AST symbols to conversations through the stored `first_conv_id` field, which a same-session audit measured at 50.7% agreement (1135 of 2237) with evidence-bearing symbol-level attribution derived from `code_evolution` change events, against a pre-registered bar of 90% — and 499 of 532 files carry a single uniform id, i.e. the field is a file-level projection rather than per-symbol evidence. The defect replicated on a second project of the same operator: 355 of 711 nodes (49.93%) agreed with event-derived attribution, against 50.7% on the primary project, under the same 90% bar (receipt R-attribution-2, operator-local). A 34-query replication of the five-arm grid on that project is quarantined because its gold failed an independent audit, and it is excluded from every estimate here. Seeds routed through a half-accurate map cannot demonstrate structural signal even where structural signal exists. The honest reading is therefore corpus- and attribution-bounded: on this corpus, with this attribution, AST structure contributes no detectable ranking signal. The rematch — the same sealed kit with the seed mapping switched to evidence-bearing attribution — is queued, and is stated as a queued experiment rather than as a defense of the null.

Relocating rather than discarding the claim: what the AST layer is measured to deliver on this corpus is _integrity_. A live integrity run recorded in the repository's code-graph analysis note of 2026-07-31 measured witness closure 98.0% (bar 90) and internal binding 85.4% (bar 70) with zero drifted edges; a sealed run of the same gates gave slightly different rates; its receipt is not in the repository, so those rates are omitted. Consumer surfaces label definition-backed versus name-only matches, carry line anchors, and abstain visibly rather than guess. These are gate properties, not MRR, and they are what makes a provenance answer trustworthy rather than merely ranked.

== Controlled contamination (E3)

From a session-zero snapshot C0 (17,134 chunks, pre-dating all evaluation dialogue) we built C1 = C0 + only the evaluation-design transcript, C-sham = C0 + an unrelated transcript matched for size (107% of C1's bytes, zero query-text occurrences), and C5 = C0 + five scripted re-ask cycles (verbatim query re-askings with paraphrased answers, explicitly marked synthetic — a controlled self-referential injection, not a natural-ecology observation). Retrieval used exact brute-force scan, eliminating ANN variance so the independent variable is corpus content alone; 8 queries whose owner-audited origins pre-date C0; three arms.

#figure(
  table(
    columns: (auto, auto, auto),
    align: (left, right, right),
    stroke: 0.5pt + rgb("#cccccc"),
    inset: 5pt,
    [*Arm under C5 (dosed re-asking)*], [*echo\@10*], [*origin-MRR*],
    [kNN], [4.9 / 10], [0.014],
    [walk $minus$ echo-defense], [4.9 / 10], [0.014],
    [*walk with echo-defense*], [*1.4 / 10*], [*0.047*],
  ),
  caption: [Contamination dose-response and repair, exact-scan retrieval, mean over 8 eligible queries.],
)

Five scripted re-ask cycles capture half of naive retrieval's top-10 (on one query the entire top-3 becomes the synthetic cycles); the undefended walk is equally captured, since hop-2 spreads outward from echo seeds. The echo defense is a quantified repair: $minus$72% echo occupancy, 3.4$times$ the origin-MRR under dose, and on one query the origin _enters_ the top-10 only in the defended arm — demoting echoes clears ranked space for provenance. The sham control is null for the kNN arm (its rankings are byte-identical to C0), so for that arm displacement is content-specific, not a corpus-size artifact; and C1 shows the evaluation-design conversation itself entering a query's top-3 — storing a single evaluation session measurably perturbs the system under evaluation. Disclosure: under exact scan on the session-zero corpus, origins sit outside the top-10 for 6–7 of 8 queries in _all_ conditions (the origin-finding floor from E2 again); the dose-response and repair rows are the load-bearing results, origin-rank trajectories are floor-limited.

== A pre-registered negative result: global ratification weighting

The E2 gold construction suggested a natural extension: if ratification behavior can grade relevance, perhaps it can weight retention. We tested whether a _global_ per-conversation ratification score — DIRECTS / ACCEPTS / REJECTS dialog-acts extracted by an LLM over each conversation, corroborated against the external git ledger, with uncorroborated scores capped at 0.6 — ranks decision origins. The pre-registered gate was Spearman correlation between backfilled scores and the sealed E2 grades (conversation-level, taking the max grade across queries), with an explicit halt condition at $rho approx 0$. The gate failed three times, across three extractor generations. Run 1 measured $rho = 0.060$ but was an extraction artifact: 88% of conversations yielded zero acts (the digest sampled mostly assistant and tool text, and the echo-hardened prompt bailed to empty output on instruction-like digests — which all agent digests are). Run 2, under a rebalanced prompt, extracted acts with genuine variance and still measured $rho = 0.071$ ($n = 123$); grade-3 sealed origins averaged a score of 0.362 against 0.422 for grade-0 incidental conversations. Run 2 was believed at the time to also use an operator-turn-prioritized digest; post-merge external review exposed that the digest builder never received author labels in production (a storage-layer join omission), so v2 had silently fallen back to head/tail sampling. After fixing the join and re-running the gate with operator-turn prioritization genuinely active, run 3 measured $rho = 0.036$ ($n = 111$; grade-3 mean 0.447 vs. grade-0 mean 0.476) — the mechanism correction made the correlation flatter, not better. Implementation conditions of the halted experiment: the backfill made 4,112 calls to a small model and about 2.29M output tokens (3,956 conversations scored); newest-first enrichment ordering starved the oldest conversations, which are the gold, and because code-evolution corroboration covered only about 106 recent sessions, 533 of 534 early scores sat on the uncorroborated cap. The results file asserts that the uniform cap left the gate unaffected, but it reports no uncapped comparison on the same rows, so the gate result holds for the capped scores.

The mechanism is a construct mismatch, not noise. The ratification score is _global_ — this conversation's work, any work, was directed and accepted — while E2 relevance is _query-conditional_ — load-bearing for _this_ decision. In a high-ship-rate solo corpus nearly every working session directs, accepts, and ships something, so node-level act-strength cannot separate "origin of this decision" from "productive session about something else." The behavioral benchmark planned downstream of the gate was not run: benchmarking retrieval on a signal measured as flat would credit policy, not recall. E2's gold worked precisely because it bound acts to a query's artifact; the retention-weight version of the thesis dropped that binding and halted at its gate. What survives is an untested _edge-level_ formulation: ratification acts bound to (conversation, artifact) pairs, weighting reinstatement graph edges and joined query-conditionally — a redesign, not a tweak, and explicitly open. A post-halt data audit ruled out the zero-cost version of that redesign: the stored extraction rows persist commit identifiers but discarded the file identity that selected them, so strict act-to-artifact binding covers zero sealed-gold pairs on existing data — any future edge-level attempt must persist the binding at extraction time.

== A blinded behavioral benchmark: re-litigation under three recall arms

The gate halted the retention-weighting program but left the behavioral question it was meant to serve — does recall over recorded history stop an agent from _re-litigating_ settled decisions? — decoupled and testable. We ran it as a three-arm benchmark under externally advised binding conditions: a similarity-only control arm was required (a no-recall control alone would only show "context helps"), the majority of tasks had to postdate the retrieval tuning they probe (avoiding train-on-test circularity), and grading had to be human and blinded.

_Design._ Sixteen tasks were sealed (SHA-256) before any arm ran: realistic teammate requests against the corpus's own settled history, each with a pre-registered correct action across four classes — _settled-for-cause_ (correct action: decline, citing the true reason; 4), _should-change_ (a recorded decision's own exit condition is now met; correct action: accept; 4), _irrelevant-past_ (superficially similar history that is not applicable precedent; correct action: judge on merits; 2), _ambiguous_ (correct action: check with the owner; 2), plus four calibration tasks. Three arms answered every task under an identical prompt and model, differing only in retrieved context: production reinstatement recall (R), similarity-only kNN (K), and no recall (N). Arms ran against a frozen snapshot scrubbed of the design session and its subagent traces (the E3 discipline applied prospectively); retrieval outputs were verified free of scrubbed identifiers; a single shared renderer produced arm-invariant context with no scores or arm-revealing wording. The 16 tasks × 3 arms produced 48 responses; the 36 responses to the 12 headline tasks were graded on a 0–3 reason-faithful-action rubric, and the four calibration tasks were scored at action level only. Grading was performed by the corpus owner (the sole author) with arm identity hidden behind per-task shuffled codes and the key sealed until all grades were recorded — blind-to-arm, but not independent of the author; we label it _author-graded, blinded_ rather than independent human evaluation.

_Results._ At action level the recall arms each took the pre-registered correct action on 12/16 tasks against 8/16 for no recall — and R and K produced _identical_ verdicts on all sixteen. On blinded reason quality (12 headline tasks, max 36): R 27, K 27, N 24. Class-level grades (summed over each class):

#figure(
  table(
    columns: (auto, auto, auto, auto),
    align: (left, center, center, center),
    table.header([*Class (tasks)*], [*R*], [*K*], [*N*]),
    [settled-for-cause (4)], [10/12], [9/12], [6/12],
    [should-change (4)], [6/12], [6/12], [6/12],
    [irrelevant-past (2)], [6/6], [6/6], [6/6],
    [ambiguous (2)], [5/6], [6/6], [6/6],
    [*Total (12 headline tasks)*], [*27/36*], [*27/36*], [*24/36*],
  ),
  caption: [Class-level blinded rubric grades (0–3 per response, summed per class). Author-graded, blind to arm; the key was unsealed only after all grades were recorded. R = reinstatement, K = kNN, N = no recall. R and K tie on the total but differ on two class subtotals (settled-for-cause 10 vs. 9; ambiguous 5 vs. 6). No per-task grade table is printed because no per-task receipt exists.],
) The aggregate conceals the structure. The class scores are rubric points summed over four responses per arm, not counts of failed responses. On _settled-for-cause_ tasks the recall arms score 10 and 9 of 12 points against 6 of 12 for no recall. The sharpest single case is a calibration task, scored at action level only: the no-recall arm accepts re-introducing a shipped performance regression that both recall arms decline, citing the original incident. On _should-change_ tasks all three arms score an identical 6 of 12 points. Per the receipt, each arm accepted only one of the four should-change tasks (one acceptance per arm), which leaves nine non-acceptances across the three arms' twelve responses. The receipt's prose gives a different count of responses that asked for more information, and the response categories were not re-checked, so no deflection count is printed. The non-acceptances held even when the retrieved context contained the original decision's own exit condition — a uniform, arm-independent conservatism. Recall shifts the failure mode from _re-breaking settled decisions_ toward _over-deferring on justified change_; task mixes without a should-change class would miss this entirely. On _irrelevant-past_ tasks every arm scored perfectly — including on a task proposing the edge-level follow-up immediately after the node-level halt: no arm over-generalized the negative result into a blocker.

_Reading._ The benchmark-level result concerns _recall over recorded history_, not reinstatement specifically. On the settled-for-cause class the recall arms fall 2 and 3 points short of the 12-point maximum against 6 short for no recall, the class where Naur's theory-loss account predicts a difference; the other three classes show no recall advantage. Reinstatement earned no separation from one-shot kNN: identical verdicts on all sixteen tasks and an identical reason-quality total (27 of 36 each), with class subtotals that differ by one point on two classes, in opposite directions. These tasks are single-hop ("does history bear on this request?"). A tie on single-hop tasks shows no advantage on multi-hop ones; it motivated the hypothesis that any advantage of the walk lies in multi-hop provenance, where the +53% spike was measured. T3 below tested that hypothesis against a hybrid baseline and did not support it.

== Staleness benchmark: witness verdicts vs. deterministic baselines (T4/H1) <sec-t4>

The witness-verdict staleness rule was evaluated on labels the repository gives away for free: symbol time-travel across release tags. Between `v8.0.0` and `v9.5.0`, 13 of 17 tags are sampled deterministically (integer linspace, endpoints included) and every `(file, symbol)` witnessed at an intermediate tag is a "belief held then." The prediction rule is the T4 replay rule, not the online auditor — superseded if any later sampled tag holds a different stamp, even when the original stamp returns at the final tag (the `codewitness` auditor returns `Intact` after A$arrow$B$arrow$A), obsolete if absent at the final tag — and ground truth is defined directly off the final tag. One structural property is stated up front: the rule's later-tag scan includes the final tag, so predicted-stale is a superset of ground-truth-stale and recall 1.0 holds _by construction_; precision is the one empirically meaningful number. Five arms score the same 13,626 beliefs through one confusion-matrix implementation, with no arm-specific filtering and no LLM anywhere.

#figure(
  table(
    columns: (2.4fr, 0.8fr, 0.8fr, 0.9fr, 0.9fr, 1fr, 0.9fr, 0.7fr),
    stroke: none,
    align: (left, center, center, center, center, center, center, center),
    table.hline(),
    table.header([*Arm*], [*TP*], [*FP*], [*FN*], [*TN*], [*Precision*], [*Recall*], [*F1*]),
    table.hline(),
    [witness verdicts (witness rule)], [1,265], [0], [0], [12,361], [1.000], [1.000], [1.000],
    [grep, symmetric normalisation], [20], [0], [1,245], [12,361], [1.000], [0.016], [0.031],
    [recency 30 d], [920], [5,677], [345], [6,684], [0.139], [0.727], [0.234],
    [recency 90 d], [556], [3,020], [709], [9,341], [0.155], [0.440], [0.230],
    [recency 180 d], [0], [0], [1,265], [12,361], [0.000], [0.000], [0.000],
    table.hline(),
  ),
  caption: [T4 Tier A staleness classification, 13,626 beliefs from 11 intermediate tags, run at `5d2bd81`. Recall for the witness-verdict arm is guaranteed by construction; precision is the comparison column.],
)

_Reading._ Grep is precise but blind: all 20 of its stale predictions are correct, but it finds 20 of 1,265 stale beliefs. Recency baselines confirm what the TAD-v2 design assumes — wall-clock age is a weak staleness signal at every threshold. Witness-verdict precision 1.000 must be read with its limits: the replay rule can produce a false positive only through a stamp that changes between sampled tags and returns to its original value by the final tag, and no such case was observed (0 false positives); Tier B labels no commit a revert, but its `^Revert` subject match cannot see a change and restoration made by ordinary commits, so that count does not rule such cases out; the claim is _unfalsified on this corpus, not proven_. Determinism held: back-to-back reruns produce byte-identical metrics (provenance-stripped SHA-256 equal), and each result carries the benched repository's HEAD, the stamper binary's SHA, and per-tag stamping statistics — fields that make an empty or collision-blind path visible rather than treating a completed run as evidence of a valid label set.

_Two rigged-eval catches are part of this result, not footnotes to it._ Audits of this benchmark were explicitly directed to search for rigging in the proposed method's favour, and found it twice. First, the grep baseline normalised Rust-style `::` qualification but not dot qualification, making qualified JavaScript symbols invisible to grep while the witness rule saw the ledger identity; the repair (symmetric `::` and `.` handling) removed all 66 originally reported grep false positives, and the once-claimed witness-verdict-over-grep _precision_ gap does not stand. Second, a deeper audit found the supposed ground truth itself was collision-bound: historical extraction collected definitions by `(repo, file, kind, name)` before witness qualification, collapsing coexisting same-named definitions — ten source-intact beliefs (six `CodeContext::is_empty`, four `format_text` methods on different stats types) were labelled stale by both prediction _and_ ground truth, hiding operational false positives inside an apparent 1.000. That removed the earlier run's precision claim entirely (demonstrated ceiling 0.992 for that run). The honest response was not to hand-patch ten rows but to fix extraction before qualification — occurrence keys of `(node, span, AST ordinal)` — and rerun every arm from the same corrected labels; the table above is that rerun (belief population 13,575 $arrow.r$ 13,626, stale 1,275 $arrow.r$ 1,265). The second label tier (episode ancestry over 140 release tags: 558 reachable commits, 540 shipped, 0 labelled reverts by a `^Revert` subject match, 60 session-linked) contributes coverage only — its positive class is empty on this corpus, so reporting classifier metrics for it would be fabrication. Tier A reproduces from git history alone; the 60 session links join git attribution to transcript attribution in the system's database, so reproducing them needs the matching attribution data.

// receipt: see Appendix D (receipt R-delivered: report written at the time, scorer gone)
_A deployed-surface audit that the benchmark did not predict._ The benchmark above scores stamps at release tags. What a person or an agent actually sees is a verdict line rendered into a session-start recap or a prompt. In an audit of one operator's deliveries over the 30 days to 2026-10-08, under the build then installed, 223 of 374 distinct delivered verdict lines were judged false against git. The recorded causes (a project-keyed anchor, a receipt that cited the observed HEAD, reinstatement without an ancestry gate) are ones that release-tag replay cannot exercise; a receipt rule that re-reads committed bytes and a forward-only ancestry rule followed. The receipt class is weak: a report written at the time survives, and the scoring scripts do not (receipt R-delivered). The benchmark stamps, the delivered lines and a later fix check are three different populations, so we claim no paired before-and-after effect. Appendix D gives the full counts. The direction is the point: precision 1.000 on the benchmark population said nothing about the delivered surface.

// ==================== T3 cross-source benchmark ====================
// Receipt paths are relative to the 10.1 worktree root. Code line numbers refer to the current tree, not the July run.
== A pre-registered cross-source benchmark: the walk loses receipt lookup and does not separate on multi-hop (T3) <sec-t3>

// receipt: docs/plans/saga-t3-preregistration.md; docs/plans/saga-t3-results.md (Amendment A); detail in Appendix A
The re-litigation tie on single-hop tasks motivated a hypothesis: that reinstatement's value, if any, lies in multi-hop provenance. T3 tested that hypothesis directly, under a protocol committed before any arm ran, and did not find it: the walk lost exact receipt lookup to hybrid retrieval, and on the sealed questions, 14 of which form the primary multi-hop stratum, the arms did not separate. Three arms answered over one frozen snapshot: R, the five-step `csr_why` walk; K, one-shot hybrid retrieval that fuses a vector top-20 and a full-text top-20 by reciprocal-rank fusion (Cormack et al., 2009; the lexical channel was a fairness grant written into the protocol); and N, a no-recall arm that sees only the question. Two sets were frozen. The deep set is 27 sealed questions, 14 of them multi-hop, whose keys derive from git history, plan files and task lifecycles only. The mechanical set is 396 receipt-class pairs: each query is a commit message, and its gold is any conversation whose transcript holds that commit's hash inside a git tool result, a receipt match rather than a unique authoring session. Gate M scores recall\@5 on the mechanical set with no judge; Gate D scores the deep set with two cross-family judges blind to arm. Each gate passes if R beats K by a sign test over discordant pairs at $p < 0.05$ (for Gate D, on the multi-hop subset), and the meaning of each outcome was committed in advance. After the results were committed, 4,346 conversations that a classifier identified as hook-recursion transcripts were found in the frozen snapshot (84% of its conversations and about 37% of its chunks); that classifier-identified residue was removed and both gates were re-run. Both runs are reported: "as published" on the original snapshot and "clean" on the scrubbed one. Set construction, the one deviation caught before judging, the scrub and the released kit are in Appendix A.

// receipt: docs/plans/saga-t3-results.md:22-30 (Gate M table), 112-115 (Amendment A, Gate M table)
_Gate M fails, in the reverse direction._ As published, R placed a receipt conversation in its top 5 for 216 of 396 queries (0.545) and K for 310 (0.783). Discordant pairs were 13 R-only against 107 K-only, two-sided exact sign test $p approx 1.5 times 10^(-19)$ (the results headline rounds this to $p approx 0$). On the clean snapshot R scored 0.581 and K 0.813, with 11 R-only and 103 K-only pairs, $p approx 6.9 times 10^(-20)$. Scrubbing raised both arms and changed no conclusion. The mechanism first published with this gate (FTS matches the exact tokens while the walk dilutes them) was wrong; the ablation below replaces it.

// receipt: docs/plans/saga-t3-results.md:38-54 (Gate D table), 117-124 (Amendment A, Gate D table)
_Gate D is an underpowered null._ A question counts for an arm only when both judges agree; split verdicts are excluded (6 of 81 as published, 7 on the clean re-run), so denominators differ by arm. On the 14 multi-hop questions R answered 3 correctly in both runs, and K answered 6 as published and 4 clean. Discordant pairs were 0 R-only against 2 K-only in both runs ($p = 0.50$). On the full set R had 10 correct and K 13 as published (1 R-only, 2 K-only, $p = 1.00$); clean, R had 10 and K 11 (0 R-only, 4 K-only, $p = 0.125$). The direction mildly favours K, and two to four discordant pairs cannot separate the arms in either direction. The no-recall arm answered none of the 27 correctly. Every correct answer in the benchmark needed retrieved context. That shows recall of the recorded history matters on these questions; it says nothing in favour of the walk.

// receipt: docs/plans/saga-t3-results.md:24-27, 46-49, 112-120
#figure(
  table(
    columns: (1.6fr, 1fr, 0.7fr, 1.1fr, 1.1fr, 1fr, 1.2fr),
    stroke: none,
    align: (left, left, center, center, center, center, center),
    table.hline(),
    table.header([*Gate (n)*], [*Run*], [*N*], [*K*], [*R*], [*R-only / K-only*], [*$p$*]),
    table.hline(),
    [M, receipt lookup (396)], [as published], [n/a], [0.783 (310/396)], [0.545 (216/396)], [13 / 107], [$approx 1.5 times 10^(-19)$],
    [M, receipt lookup (396)], [clean], [n/a], [0.813], [0.581], [11 / 103], [$approx 6.9 times 10^(-20)$],
    table.hline(stroke: 0.5pt + rgb("#cccccc")),
    [D, multi-hop (14)], [as published], [0/14], [6/13], [3/13], [0 / 2], [0.50],
    [D, multi-hop (14)], [clean], [0/14], [4/12], [3/13], [0 / 2], [0.50],
    [D, full set (27)], [as published], [0/27], [13/24], [10/24], [1 / 2], [1.00],
    [D, full set (27)], [clean], [0/27], [11/22], [10/25], [0 / 4], [0.125],
    table.hline(),
  ),
  caption: [T3 gate results. Gate M is recall\@5 of a receipt conversation. Gate D is consensus-correct answers over the questions on which both judges agree, so denominators fall by the split verdicts. "As published" is the original frozen snapshot; "clean" is the re-run on the snapshot scrubbed of hook-recursion transcripts (Gate D no-recall answers reused). Sign tests are two-sided exact over R-only / K-only discordant pairs. N = no-recall arm, K = hybrid vector + FTS, R = reinstatement walk. The multi-hop subset is the pre-registered Gate D test.],
) <tab-t3-gates>

// receipt: docs/plans/saga-t3-results.md:126-151 (Amendment B); detail and table in Appendix A
_Component ablation (post-hoc, exploratory)._ On the clean snapshot K's vector channel alone scores 0.697, its full-text channel alone 0.162, the fusion 0.813, and the walk 0.581, which is 12 points below the vector channel that seeds it. A post-hoc trace of the 103 queries that only K answered (Appendix A) proposes two mechanisms, neither tested by an intervention: echo-aware seed selection (63 of the 103) and the stacked query-echo and scaffold demotions (40 of the 103). On this diagnosis the defenses that repair contamination in E3 demote the answer on exact-key queries, so contamination defense and receipt lookup compete for the same signal, which argues for routing by query class rather than one retrieval policy for every query. What T3 leaves standing (recall against no recall, and the 423 of 529 commits linked by exact-key receipts) is also in Appendix A.

// receipt: docs/plans/saga-t3-preregistration.md:110-118; docs/plans/saga-t3-results.md:9-20; docs/plans/saga-reinstatement-spike.md:36; docs/plans/saga-t3-results.md:132-137; this paper, Baseline for comparison (Arm A)
_The pre-committed failure interpretation applies._ Gate M failed and Gate D did not pass. The pre-registration stated the meaning of this outcome before any arm ran: reinstatement's multi-hop advantage is not supported on this corpus, and the claim reduces to the provenance-aware rerank, the ledger's hygiene and the receipts layer. We report that as the result. On this benchmark the walk does not beat hybrid retrieval, either on the multi-hop questions it was built for or on receipt lookup. This bears on the earlier results. The +53% coverage win was measured against Arm A, one-shot cosine kNN over chunks and reflections, which has no lexical channel. T3's K arm adds that channel, and on receipt lookup even K's vector channel alone beats the walk (0.697 against 0.581, exploratory). The spike's coverage win stands as measured, but it compares the walk with a weaker baseline than T3's, and it does not show an advantage over hybrid retrieval.

_Limits._ One operator's recorded history from two of that operator's repositories; answers from one model family; two judges, with split verdicts dropped; Gate D contexts in which each retrieved item is cut to a 200-character excerpt; a Gate M gold built from commit hashes, which favours an arm with a lexical channel; and an ablation that is post-hoc. With two to four discordant pairs per comparison the Gate D result is a null, not evidence that the arms are equivalent; the full-set null holds for complete-case pairs only and is sensitive to how the split verdicts would be adjudicated. The full statement of limits is in Appendix A.

// ==================== 6. Field measurements ====================


= Field measurements on the operator's store (October–November 2026)

Between 2026-10-13 and 2026-11-17 we ran a set of probes on the operator's own store, transcript mirror and hook logs. They are not one study of an installed release: they ran on different builds, on different dates and with different inclusion rules, in three groups. The first group is audits made while 10.1 was being built, on pre-release builds and the store they wrote: the scrub run on a clone (2026-10-13), shell-made edits over the 14 days to 2026-10-19 (all sessions), empty results from 2026-10-22, the warm-call timing (2026-10-23), the stage timing, cross-CPU check, echo checkpoint and confidence labels on rewritten-core builds (2026-10-27), and index recall by generation (2026-10-26 to 2026-11-09). The second group is two compaction audits: the host-metadata audit of 2026-11-04 over every boundary since about 2026-10-20, and the historical study of main-thread boundaries from 2026-10-05 to 2026-11-05. The third group runs from 2026-11-14 to 2026-11-17: a six-week extract of calls to this system's tools ending 2026-11-14, the probes of 2026-11-16 on a frozen nightly copy of the store and on the transcript mirror (the shipped-engine bank runs they use were recorded on 2026-11-10), and the send-gate replay of candidate build `d953e184` on 2026-11-17. Each table row states its own population, and the Evidence register gives each receipt's date. The probes ask whether the mechanisms in this paper reach the agent in ordinary work, and what agents do instead. Most results are nulls or negatives. We report them because they bound the claims made above: several mechanisms behave correctly where they were measured, but they are rarely reached, rarely called, or not distinguishable from a control.

== Method and scope

// BLOCK K32
// receipt: agent-log-testbed/2026-11-16-verdict-review/angles/RESULT.md:21-25
All probes use one operator's store, transcripts and logs; the cross-CPU check also used a second machine. Headless runs dominate the population. On 2026-11-16 the transcript mirror held 19,642 top-level files. Of these, 19,268 carried the headless entrypoint (`sdk-cli`): narrator runs and other headless runs. 272 files met the proxy we use for a human-driven session, the interactive CLI entrypoint and a compressed size of at least 20 KB. The remaining 102 were not assigned to either group. The proxy is imperfect, because a script can drive the interactive CLI. The mirror is also not the search index, so mirror counts and index counts are different denominators. The probes of 2026-11-16 filter to human-driven sessions unless their table row says otherwise; the other rows state their own inclusion rule.

// receipt: PREREG.md in each folder of agent-log-testbed/2026-11-16-verdict-review/ and agent-log-testbed/2026-11-17 block-audit folder/{blocks,novelty}/
The receipts are operator-local files that are not released, because they derive from private transcripts. A reader cannot re-run them, and they carry less weight than the repository-receipted experiments above. The Evidence register lists each key and its class. Most November probes have a pre-registration file written before scoring, some with dated amendments. The folders are not externally timestamped, so "registered" here is self-declared. The Status column marks registered readouts, and the Result column marks post-hoc steps. Usefulness and sufficiency labels are one reader's reading unless a row says otherwise.

== What the probes found, in brief

The full table (every probe with its n, interval, status and receipt key) and the per-probe caveats are in Appendix B. The results that bear most directly on this paper's claims follow.

- _Pushed context._ Next-turn identifier overlap for chunks shown at prompt time was 21 of 1,502 (1.40%), against 22 of 1,502 (1.46%) for the next-ranked chunks of the same project that were not shown, a control reconstructed by cosine (difference $minus$0.07 pp, 95% CI $minus$0.88 to +0.68, 74 sessions). The label is a crude lexical proxy and exposure was not randomized. The prompt-time send gate missed its own bar in a replay of one candidate build: 74 of 171 sent blocks useful (43.3%) against 55%.
- _After a compaction._ Of 2,208 detected cases in which a pre-compaction detail was needed again, the host re-attached the file in 35% and the agent read or searched the workspace in 52%; recorded history accounted for at most 6.5%. The registered cost comparison is inconclusive. The recovery tool was called in 1 of 303 post-compaction windows, before the pointer named it. Directive presence in summaries fell across compaction generations (99%, 50%, then 22% by a strict detector or 37% by a loose one at the third), with a weak detector and no harm shown.
- _Retrieval in use._ Code-history lookups occurred in 75 of 134 human-driven code-editing sessions; git answered 19 of 20 sampled ones; `csr_why` accounted for 9 of 605 tool calls in a six-week extract. The frozen recall bank has 0 of 200 forms that name a file or symbol, against 12 of 703 real searches; on that bank the shipped engine's recall\@5 is 1.00 for verbatim forms and 0.22 for paraphrases. Single time priors moved MRR by under 0.03 on 330 real late needs.
- _Verdict reach._ In one snapshot at most 1,710 of 1,052,345 chunks carried a witness binding.
- _Ingestion audits._ For edits made through the shell, file history returned one apparent match in 574 cases, which the audit treated as a likely false positive; for edit-tool writes it found 390 of 408. This paper's coverage ground truth is built from the same table. One marker-based scrub of a clone dropped 552 chunks, at least 322 of them real content.
- _Evaluation controls._ The same reranker gave different rank-1 results on two CPUs (231 of 324 agree for the untuned base model; joint masking of the matrix extensions restores 324 of 324), so a judged packet is valid only for the backend that produced it. And an index-lookup timer is not a tool-call time: in one stage audit of 323 calls the p50 of the index lookup was 1.6 ms and the p50 of total engine time was 712.7 ms.

== What these results do to the paper's claims

Evidence helps only where it is reachable and used, and the field probes found both conditions weak. Next-turn identifier overlap with pushed context showed no excess over a similarity-matched, reconstructed control. After a compaction, the workspace, through host re-attachment and the agent's own reads, supplied most detected late detail, and recorded history supplied a small share. Git answered most sampled code-history lookups, and the dedicated history tool was rarely called. The frozen recall bank has no form that explicitly names a file or symbol, and the field searches we sampled are mostly keyword sketches; 12 of 703 named a file or symbol. These are single-operator observations on several builds, with operator-local receipts; they bound the claims above and do not replace a controlled study.

// ==================== 7. Discussion ====================
= Discussion

== What the results support

The results support a narrow claim. Against one-shot _cosine_ kNN — a baseline with no lexical channel — reinstatement recall improves provenance coverage at equal budget on these corpora. Against hybrid kNN+FTS retrieval, the pre-registered T3 benchmark found no multi-hop separation on 27 sealed questions and a reversal on 396 receipt-lookup queries (0.813 against 0.581), so an advantage of the walk over flat retrieval is not supported once the baseline has a lexical channel. The earlier wins stand as measured; what they measure is smaller than "reinstatement beats flat retrieval". The detail of those wins follows. The +53% spike lift cleared a pre-registered +25% gate with zero per-query losses, equal diversity (M2), and qualitative origin-rescue on the canonical failure case (M3). Mechanism attribution requires care, because it moved as the gold improved. Raw channel counting against file-touch coverage placed most GT hits in the TCM-style blend channel; the controlled ablation against decision-graded gold reverses this as a within-grid ordering — the co-edit graph-only arm scores above the historical full walk on every graded metric, on partly unaudited grades and without intervals. This reversal also changes the shape of the circularity defense: blend-dominance was the original argument that the coverage win was not an artifact of the graph channel sharing plumbing with the file-touch GT. That argument no longer carries the weight, and the graded gold only narrows the concern: grades derive from dialog-acts, sealed origins and ship-event ledgers, but the pool that was graded includes file-touch history from the same `code_evolution` table the graph channel reads, so graph-channel dominance on it is less self-confirming than against file-touch coverage, not independent of it. A corollary we own explicitly: the shipped `csr_why` default is the fused walk, and the ablation shows a blend-free graph-heavy configuration outscoring July's historical full walk on graded gold; E1 did not measure the 10.1.0 walk with its structural routing and quotas, and the deployed configuration was provisional pending the fusion re-weighting identified in E1. That re-weighting was never done. The walk that shipped in 10.1.0 changed in other ways instead — a query symbol that resolves in the AST layer switches off co-edit spread, structural seeds launch only where attribution or incident edges name a conversation, and an episode quota was added (System Design) — and none of those changes has been measured against the graded gold. The walk ships as an LLM-free MCP tool; no latency figure is reported for it.

This is a partial, machine-assisted answer to Naur's theory-loss problem for agentic coding: when theory lives in the intent trace of a saga, retrieval that only matches surface similarity loses the theory (Naur, 1985; Howard & Kahana, 2002). Whether retrieval that reinhabits seed context and spreads through artifact links recovers it _better than a well-built hybrid retriever_ is, after T3, an open question rather than a result.

== Observer effect / corpus self-contamination

We name the phenomenon _self-indexing evaluation contamination_ (retaining "Heisenberg-like observer effect" as the informal gloss). A methodological observation at least as consequential as the coverage table: *measuring a self-indexing system changes the system*. CSR's hooks import the evaluation session while the evaluation is designed and discussed. Near-verbatim restatements of queries and spike dumps become high-similarity competitors to origin conversations — exactly the failure mode one-shot retrieval already suffers, now amplified by the experimenters themselves.

Two consequences follow:

+ *Method.* Any evaluation of a self-recording agent system must run against a *frozen corpus snapshot* taken _before_ the evaluation session can be captured and re-imported. Same-day "freeze after discussion" is insufficient. Prefer a session-zero backup. Report both clean and contaminated numbers when contamination is unavoidable; use the gap as a diagnostic, not as a reason to discard the clean run. A second, smaller comparability requirement surfaced during regression testing: ANN index construction is not deterministic across rebuilds (HNSW insertion effects), and per-query coverage counts near score boundaries can shift between two indexes built from the identical frozen corpus (seen once during regression testing, while two binaries gave per-query-identical results on the _same_ build; the per-run counts were not committed). A/B comparisons must therefore share one index build, not just one corpus.
+ *Design.* Phase 1.5 applied provenance-aware reranking to the reinstatement pool inside `csr_why`. The hypothesis — that echo demotion specifically counters contamination — is supported by receipted evidence in two places: the E1 ablation (removing the echo defense drops the full arm below the kNN baseline) and the E3 dose-response (under dose, the defended arm shows 72% less echo occupancy than the undefended arm, and a size-matched sham left the kNN rankings unchanged). Chunk-level demotion does not rescue decisions whose re-askings dominate whole conversations, and a post-hoc trace in T3 proposes, without an intervention test, that the same demotions invert on exact-key queries. Notably, two rerank heuristics that work in general-purpose search (`reflect_on_past`) were _rejected_ by evidence in the provenance task: tool-mechanic chunks are evidence there, not noise, and flat user-authority boosts promote weak user chatter over strong evidence. Rerank policies are task-relative.

We hypothesize self-indexing evaluation contamination is a class-level issue rather than a CSR-only bug — the mechanism requires only that an agent index its own tool use while researchers discuss eval items, which would inject high-similarity confounds in any such system — but only one implementation has been tested; replication on other self-recording stacks is open.

A third instance surfaced during the v10 work, and it is the purest form of the class: the system was indexing _its own retrieval output_. CSR's importer embedded its own MCP tool calls and results, and its hook-injected context blocks, as ordinary conversation content. Measured on the live corpus, 747 conversations carried CSR-injected material — memory-manifest headers, past-context blocks, episode indexes — as searchable, high-similarity text. The loop is closed contamination: what the system says to the agent becomes what the system later retrieves. The fix follows the paper's own prescription rather than fuzzy filtering: exact identity-based suppression of the system's own tool payloads (with id-correlated results; unresolvable identities are kept, since false suppression of user content is worse than residual contamination) and cutting of complete hook-injected wrappers, applied through one shared sanitizer across the reflection-producing pipelines (the next paragraph narrows what is cut). The audit trail here repeated the silently-inert lesson above in inverted form: adversarial review found that the sanitizer initially did not cover every pipeline (episode extraction in the stop hook still read raw transcripts) — a partial fix that unit tests passed and only pipeline enumeration caught.

Two corrections to that account, and one incident, belong here. First, the fix is narrower than "scrubbing" suggests. Only _structural_ items are cut: the system's own tool calls and complete injected wrappers. Text that merely matches the system's output grammar is labelled and sunk, never deleted, because deleting was tried and destroyed evidence: one marker-based scrub of a 733,388-chunk clone dropped 552 chunks, at least 322 of which were code listings, tool output or analyses that only quoted a marker (an author-classified lower bound from one run, not an error rate; receipt R-scrub). Second, the shipped ranker also removes from the candidate pool the prose of sessions that ran the system's own benchmarks and probes, judged by what else the session did rather than by the candidate's text; the filter's precision and its false suppression of genuine work are unmeasured. A separate whole-stack comparison at one checkpoint of the rewritten core saw echo at rank 1 fall from 33 to 19 of 310 keyed real queries (receipt R-adjudicator). The receipt does not establish that the shipped filter was part of that checkpoint, so the change is not evidence for it. The incident: before the system's headless model children were isolated from tools, settings and session persistence, one plugin hook fired 21,116 times in a month. The project changelog records the count; no primary count artifact exists. It belongs to the same recursion class as the T3 contamination vector.

== Silently-inert mechanisms: a second class-level failure

Preparing this paper surfaced the same failure shape twice, in unrelated mechanisms: a shipped component that does nothing, invisibly, while every unit test passes. First, the v2 ratification extractor's operator-turn digest never fired in production — a storage-layer reconstruction hardcoded a default author label, so the digest builder's operator filter matched nothing and extraction silently fell back to head/tail sampling; the run was reported (and believed) as testing the prioritized digest. Second, the resolution ledger's write contract keys on chunk identifiers, but the search renderer originally never emitted them — verdicts written exactly per the tool's own documentation would have silently no-opped. In both cases unit tests were green throughout: each layer honored its local contract while the composition was dead. Both were caught only by end-to-end replay of the real workflow (an external reviewer tracing the storage join; a live replay of the dogfooding queries through the production MCP client). We take this as a methodological finding on par with echo contamination: in systems that feed an agent its own history, where a mechanism's output feeds a model that produces _plausible output regardless_, inert mechanisms do not crash — they degrade invisibly. End-to-end replay of the deployed path, not unit coverage, is the detection instrument; and when a mechanism is corrected, its experiment must be re-run (here, re-running the gate made the negative result stronger, which is the honest outcome working as intended).

A third instance was found while auditing the shipped code for this paper, by reading rather than by any failing test. The design assumed that logging MCP-path retrievals "unblocks" usage-weighted decay. As built in 10.1.0 it cannot: MCP retrieval events are written under a sentinel session identifier that never receives an outcome, the only outcome value the code writes is success, and the default ranking policy skips the usage term altogether (`csr-engine/src/search/decay.rs`; a reading of the code, with no live count taken). Events are logged and the decay function is unit-tested; the composition can never change a score. Learned decay was therefore never "blocked on log accumulation"; the blocker is structural.

A fourth is a path that has not been shown to do anything. The carried-thread retirement path (System Design) was replayed offline over 24 carried threads in seven sessions under a copy of its prompt and flagged none (receipt R-retire). Zero flagged is not zero wrong, and it is not evidence that the path works; its live eligibility, naming, rejection and retirement rates are unknown. We list these examples one by one, each with its own endpoint, and make no estimate of how common the class is.

== One ambition, two mechanisms, one measured winner

The ratification programme and witness verdicts pursue the same ambition: prevent a system that recalls its own history from presenting historically relevant but no-longer-current material as if it described present code. They differ in where they locate the verdict. Generative consolidation placed it in an LLM extraction of conversational acts and tried to turn a global per-conversation ratification score into a retention weight; it failed its pre-registered gate ($rho = 0.036$ against sealed relevance grades, after the digest defect was corrected and the intended path genuinely ran), and its use in ranking was halted with the negative result published. (The halt covers ranking only: in 10.1.0 the extractor still runs by default as a background model call and its score is attached to results as a shadow field that no ranking reads. Given a measured $rho$ near zero, that spend buys nothing we can show, and we list turning it off as an open item rather than defend it.) Its failure was a construct mismatch: "productive or accepted conversation" is global, whereas "still load-bearing for this artifact" is conditional on a particular claim and artifact.

The witness-verdict mechanism keeps the ambition and changes the mechanism. It does not ask a generator whether a belief _sounds_ superseded; it joins the claim to an append-only span witness, compares hashes at commits, and abstains when closure or identity is not established. Under adversarial replay the repository-evidence mechanism survives exactly the cases that break conversational inference: exact reverts reinstate, non-identical A$arrow$B$arrow$A′ histories abstain, squash and cherry-pick identity is not reduced to wall-clock order. The contrast validates the contamination thesis in a stricter form: a generator reading a self-recorded corpus is exposed to the same echoes, scaffolds, and silently-inert joins as retrieval, while repository receipts are an independent evidential channel. One ambition; two mechanisms measured; the inference-based one failed its registered gate and the evidence-grounded one passed its substrate gates and the T4 benchmark above — with its end-task retrieval effect still unmeasured, and stated as such. Two later observations temper "passed". The benchmark's population (symbol stamps at release tags) is not the deployed population: an audit of the verdict lines actually delivered to sessions, on a different population and under an earlier build, judged most of them false (@sec-t4). And the mechanism's reach is small (System Design, Reach). An independent evidential channel is necessary for a trustworthy verdict; it is not sufficient for a useful one.
== From measurement to mechanism: asserted state, not inferred state

The ratification negative result did productive work in the deployed system. A dogfooding incident posed the design problem: an agent queried its recorded history for "items queued for a future binary" and four of five recalled items had already shipped in later sessions — retrieval was semantically correct, but the system indexes _prose, not state_, so a "queued" statement stays retrievable forever with no marker that a later session shipped it. The obvious fix — infer closure or supersession from conversation prose — is precisely the construct our pre-registered gate had just measured as unreliable ($rho approx 0.04$–$0.07$ across three extractors). The negative result forbade the inference path, and the shipped design is its constructive consequence: an _asserted-state resolution ledger_. The verdict about whether a remembered item is still true already exists, in-session, at verification time — the agent just checked the repository; the ledger gives that verdict a durable home instead of letting it evaporate as prose. A resolution tool records status (resolved / still-open / regressed) with mandatory cited evidence into an append-only ledger; in the evaluated build, every returned chunk was annotated at search time with its latest verdict and resolved chunks were demoted _within_ the returned page (nothing is dropped or deleted); a regression appends a re-opening row that restores full rank. The epistemic division of labor is the point: retrieval stays similarity-based and complete, while _state_ is only ever asserted, with evidence attached, by a party that just performed a verification — the system never claims to know an item is closed, it replays a cited claim that it was. In the evaluated build that party was the agent, and resolved items were demoted within the returned page. In 10.1.0 authority moved one step further out. Within `csr_resolve` the agent _proposes_ a verdict and the tool asks the person to confirm it; only a user-confirmed verdict can affect ranking and the recap, while an unconfirmed agent verdict is kept as an observation that can annotate a result but supersedes and ranks nothing (`csr-engine/src/mcp/mod.rs`). A task the host records as completed that matches a chunk with an open verdict becomes a _proposal_, which a person must promote. Separately, one partition applied before ranking receives confirmed verdicts, frozen user decisions, witness verdicts and promoted proposals, and sinks the items they supersede. Whether any of this improves what agents do is unmeasured. The proposal match is lexical — exactly the kind of prose inference the ratification result warns against — which is why it can only propose. Conversation threads, which have no ledger identity, get a third and weaker path: a model's claim that a thread is done is accepted only into a labelled, reversible guessed-done state, and only when code finds the model's quoted evidence in a newer compaction summary (System Design). That corroborates the presence of a quote. It is not an assertion by a verifier, and it justifies less than either ledger. This is a truth-maintenance move (Doyle, 1979) rather than a belief-revision one (Alchourrón et al., 1985): justifications are recorded and replayed, never inferred, and demote-don't-delete preserves the full history that regression detection and provenance need. Together with the re-litigation benchmark the ledger brackets the staleness problem — the benchmark shows recall preserves _why things were settled_; the ledger makes it knowable _when they stopped being true_. Honest limits: a single dogfooding incident motivates the design; there is no controlled study of ledger benefit yet; and its value depends on verifying agents actually writing verdicts, a workflow property rather than a mechanism property.

*Three live sightings during the preparation of this paper.* The effect was not confined to the benchmark. (1) The attempted VCS-trailer GT construction was contaminated by its own extraction step (see Method): running `git log` inside an indexed session imported the trailer IDs being extracted. (2) A file-history lookup made while drafting this paper returned, as its top hit, the session drafting this paper. (3) Most instructively, CSR's own context-injection hook exhibited the failure in its selection layer: asked whether session-start continuity had captured the current work, the hook's semantic intent classifier matched the prompt to a days-old episode consisting of _asking about that same hook_, at a similarity right at its acceptance threshold (an observation recorded at the time, with no committed receipt), and attached its resume-context block to the stale episode instead of the live thread. The injection path had not received the Phase 1.5 echo defenses; the provenance tool had. (Since fixed: in 10.1.0 the prompt-submit hook skips echo episodes, self-echo chunks and text the system itself wrote when it selects what to inject.) The general lesson: in a self-indexing system, _every_ retrieval surface — benchmark, analysis tooling, and context injection alike — needs echo defense and margin gating, because each one is a place where the system's record of being asked can outrank the thing that was asked about.

== Limitations

The full list, items (a) to (l), is in Appendix F; none is dropped here. In brief. The retrieval experiments draw on one operator's recorded history, with small query sets. The coverage ground truth shares plumbing with the walk's graph channel, and edits made through the shell were missing from both. The graded gold is only partly audited, and the pool that was graded shares file-touch history with the graph channel. LLM judges are used only in T3, with split verdicts dropped. The observer effect is mitigated, not solved. The shipped system is not the evaluated system, and no experiment compares the shipped walk with the shipped recall path. Only results whose receipts are in the record are reported. The field measurements rest on operator-local, unreleased receipts, apart from one repair figure that is a public changelog statement. And readings that a model produces for the person carry no accuracy measurement.

== Deferred work and theoretical next steps

The full queue is in Appendix G. Its head: (1) compare the shipped walk with the shipped hybrid, cross-encoded recall path on the sealed gold sets, with routing by query class as the design under test; (2) run the AST rematch on the shipped two-channel attribution, since the shipped walk already routes symbol-naming queries through that layer without evidence; (3) repair usage-weighted decay, which as built cannot change a score; (4) replay delivered verdict lines blind and paired, before and after the receipt rule; (5) exclude echoes at the level of whole conversations; (6) validate on other operators' histories. Five further questions raised by the field work are named in Appendix D with the measurement each needs.

// ==================== 8. Conclusion ====================
= Conclusion

We introduced _three-trace sagas_ — joint episodic structure over intent, deliberation, and artifact in agentic software construction — and _reinstatement recall_, a five-step, LLM-free retrieval algorithm that seeds with kNN, blends query and seed context (TCM analogue), spreads over a session–file code graph, hops episode chains, and fuses results under a provenance-aware rerank. On CSR's own development corpus, a pre-registered spike showed +53% summed GT-session coverage at $k = 10$ versus one-shot cosine kNN. (An early channel count attributed most of that gain to the semantic context blend; the graded-gold ablation reversed that attribution to co-edit graph spread, and the sham-controlled follow-up could not separate that channel from zero.) The walk was reimplemented in production as `csr_why`.

*Supported on these corpora, at the retrieval-metric level, against a cosine-only baseline:* reinstatement beats one-shot cosine kNN on provenance coverage, without an LLM in the loop — supported by GT coverage on the primary corpus (+53%, 7 per-query wins and no losses in the pre-registered spike) and by ratification-derived graded gold (+37% origin-MRR, +29% `nDCG@10`, +25% graded recall — point estimates on n=12 owner-sealed origins, without interval estimates on those lifts, on grades of which only the five injected-origin maps were audited; intervals exist for the expansion-channel contrasts only, from the E1b follow-up's 10,000-resample paired bootstrap at seed 20260731, and the full seven-arm grid remains unbracketed). On graded gold the channel ablation attributes the gain to code-graph spread — the session↔file co-edit trace, reversing the coverage-era blend attribution, with the graded pool sharing file-touch history with the graph channel — as a within-grid ranking on decision-graded gold rather than a causal attribution: the sham-controlled follow-up measures that channel's marginal contribution as directionally positive but not separable from zero (+0.017 origin-MRR, 95% CI lower bound exactly 0.000), and finds the AST-structural arm a strict null against both the base arm and its own degree-preserving sham. In the same grid, removing the echo defense lowered the full arm's origin-MRR from 0.276 to 0.234, below the grid's kNN baseline (a within-grid point estimate); controlled contamination dosing shows five re-ask cycles capture half of naive retrieval's top-10 and that the same defense removes 72% of echo occupancy at 3.4$times$ the origin-MRR, while a size-matched sham left kNN rankings byte-identical. A blinded three-arm behavioral benchmark adds a benchmark-level claim: on settled-for-cause tasks the two recall arms scored 10 and 9 of 12 rubric points against 6 of 12 for no recall (sealed tasks; author-graded, blind to arm; points summed over four responses per arm), with walk and kNN tied on verdicts and on the 27-of-36 total, and on a calibration task the no-recall arm accepted the re-introduction of a shipped regression that both recall arms declined for the recorded reason.

*Measured as a tie:* on single-hop re-litigation tasks, reinstatement and one-shot kNN produced identical actions on all sixteen tasks and identical blinded reason-quality totals (27–27). The tie motivated the hypothesis that any advantage of the walk lies in multi-hop questions; T3 then tested that hypothesis against a hybrid baseline and did not support it.

*Measured against the walk:* on the pre-registered T3 benchmark, hybrid kNN+FTS retrieval beats the walk on 396 receipt-lookup queries (recall\@5 0.813 against 0.581 on the scrubbed corpus, $p approx 6.9 times 10^(-20)$), and the two do not separate on 27 sealed cross-source questions (multi-hop subset 3/13 against 4/12, $p = 0.50$, an underpowered null; the no-recall arm scores 0/27). A post-hoc ablation and code trace of the 103 receipt queries the hybrid arm found and the walk missed propose, as an untested diagnosis, that demotions which repair contamination on conversational queries demote the answer on exact-key queries. Gate D does not support a multi-hop advantage of the walk over flat retrieval, and routing by query class is the next thing to test. The re-litigation benchmark also surfaced an arm-independent conservatism: on should-change tasks each arm accepted only one of four justified changes (nine non-acceptances across the three arms), even where the retrieved context held the decision's own exit condition; recall does not cause it but also does not cure it.

*Not yet proven:* learned usage-weighted decay; sidechain-weighted scoring; chunk-sequence temporal reinstatement; generalization beyond a single operator's history; benefit of the asserted-state resolution ledger (motivated by one dogfooding incident, no controlled study); and origin recovery itself — both E2 arms miss many of the verified origin conversations at k = 10 (the receipt's two joint-miss counts are not reconciled, so none is given), and re-askings that dominate whole conversations remain unrescued by chunk-level defenses.

*Evidence-grounded forgetting (deterministic witness verdicts):* an append-only span-witness ledger with deterministic, abstention-first staleness verdicts and demote-plus-annotate consumption. On the T4 replay benchmark (13,626 beliefs, five arms, one scorer) the witness rule classifies staleness at precision 1.000 where fair grep achieves recall 0.016 and recency baselines peak at F1 0.234 — stated with its limits (recall by construction; precision unfalsified, with zero false positives observed, and a zero count of `^Revert` subjects does not rule out change-and-restore histories; two rigged-eval defects caught by directed adversarial audit and corrected before these numbers were admitted). Its end-task retrieval effect is the open experiment, and the benchmark population is not the delivered surface.

*Observed in the field, on one operator's logs, with local and unreleased receipts:* context pushed at prompt time showed no more next-turn identifier overlap than a same-project control that was not shown (21 against 22 of 1,502); the prompt-time send gate missed its usefulness bar (74 of 171 useful, bar 55%); after a compaction, detected late-needed detail came back mostly through host re-attachment (35%) and agent file reads (52%) of 2,208 events, with the registered cost readout inconclusive; git answered 19 of 20 sampled code-history lookups, and `csr_why` accounted for 9 of 605 tool calls in one extract; the frozen recall bank had 0 of 200 forms that name a file or symbol; at most 1,710 of 1,052,345 chunks carried a witness binding in one snapshot; and an audit under an earlier build judged 223 of 374 delivered verdict lines false. None of these is a controlled experiment. Each bounds a claim above.



*Ruled out by pre-registration:* global per-conversation ratification weighting as a retention signal — Spearman against the sealed graded gold measured $rho = 0.060 arrow.r 0.071 arrow.r 0.036$ across three extractor versions ($n = 111$–$123$), halting that program at its pre-registered gate; the result survived a post-hoc mechanism correction (a digest bug found in external review), and only the edge-level, query-conditional formulation remains open.

*Immediate next steps:* conversation-level echo exclusion and `supersedes` population; enforce session-zero frozen snapshots for all future evals; an evaluation of the shipped walk against the shipped hybrid, cross-encoded recall path on the sealed gold sets, with routing by query class as the design under test; repair of usage-weighted decay, which as built cannot change a score (MCP retrieval events never receive an outcome), before any claim about learned decay; a multi-hop behavioral benchmark (tasks whose justification chain spans multiple conversations — the regime where reinstatement could still separate from hybrid retrieval, if it separates anywhere); a controlled staleness study of the resolution ledger (replay historical sessions, measure stale-recall rate with and without verdicts); the AST rematch — re-running the sealed E1b kit with symbol seeds mapped through evidence-bearing two-channel attribution (`code_node_attribution`: transcript and git) instead of projected `first_conv_id` — together with method-edge resolution for the largest unresolved edge class and `repo_defs` densification toward a repo-complete structural corpus; a neutral-prompt judge-panel replication; a small multi-annotator regrade of the behavioral rubric; validate on external agentic coding corpora.

For agentic construction, Naur's theory is no longer only in human heads — it is stranded across sagas. Systems that index only similar text will keep losing it. Whether reinstating the encoding context of the work recovers it better than a well-built hybrid retriever is not something these results establish; what they do establish is how such a claim has to be tested — frozen snapshots, a baseline with a lexical channel, sham controls, and receipts.

// ==================== Data availability ====================
#heading(numbering: none)[Data availability and privacy]

The evaluation corpus is one operator's private development history — session transcripts, subagent traces, and code evolution across personal and commercial projects — and cannot be released as-is. Transcript excerpts quoted in this paper were reviewed by the corpus owner before inclusion. The evaluation _harness_ (frozen-snapshot protocol, MCP probe scripts, blinded judging packets, agreement computation) contains no corpus content and is releasable; external replication therefore requires running the harness on one's own agent history, which is exactly the deployment condition the system targets. The Phase-2 harness is released: the E2/E1/E3 extraction prompts, grading and scoring scripts, and protocol specs are available at `csr-engine/eval-kit/` in the repository (sanitized; sealed gold labels and private transcripts excluded).

The T4 staleness benchmark is fully releasable: `codewitness bench` derives Tier A and all five arms from any git repository's own history, with no private corpus, and `codewitness labels` derives Tier B's release and revert labels the same way; only Tier B's 60 session links need the matching attribution data from the system's database. Runs are deterministic (byte-identical metrics across reruns), and every result carries a provenance block — repository HEAD at run time, stamper-binary SHA-256, and per-tag stamping statistics. The harness, this paper's result files, and the per-tag detail are at `csr-engine/eval-kit/t4/`; the retired Python label/replay scripts produce no paper numbers.

Three receipt classes are used in this paper, and the Evidence register below assigns one to every field result. _Repository files_ (`docs/plans/saga-*.md`, `csr-engine/eval-kit/`) can be checked by any reader. _Operator-local files_ derive from private transcripts and are not released; a reader cannot re-run them, and results that rest on them are labelled as field observations, not as experiments. _Reports written at the time_ survive where the script or output that produced a number does not; the paper says so wherever it prints such a number. A result with no receipt of any class is not reported.

// ==================== References ====================
#v(1em)
#text(size: 14pt, weight: "bold")[References]
#v(0.75em)

#set text(size: 10pt)
#set par(justify: false)

Agent Trace. (2026). _Agent Trace: An open specification for tracking AI-generated code_, version 0.1.0. Cursor. https://agent-trace.dev/

#v(0.5em)
Alchourrón, C. E., Gärdenfors, P., & Makinson, D. (1985). On the logic of theory change: Partial meet contraction and revision functions. _Journal of Symbolic Logic, 50_(2), 510–530.

#v(0.5em)
Anderson, J. R., & Schooler, L. J. (1991). Reflections of the environment in memory. _Psychological Science, 2_(6), 396–408.

#v(0.5em)
Baklanov, M., Barkan, O., & Koenigstein, N. (2026). CounterMem: Counterfactual evaluation of non-parametric memory use in retrieval-augmented generation. _Findings of the Association for Computational Linguistics: EMNLP 2026_, 22332–22348.

#v(0.5em)
Chen, J., Chen, L., Chen, Y., Dou, S., Zhang, M., Ge, C., Ni, S., Gui, T., Zhang, Q., Huang, X., & Pan, M. (2026). CompAct-Bench: A benchmark for working-context compaction in long-horizon agent tasks. _arXiv preprint_ arXiv:2611.25673.

#v(0.5em)
Chen, S. (2026). Governance decay: How context compaction silently erases safety constraints in long-horizon LLM agents. _arXiv preprint_ arXiv:2606.22528.

#v(0.5em)
Cormack, G. V., Clarke, C. L. A., & Büttcher, S. (2009). Reciprocal rank fusion outperforms Condorcet and individual rank learning methods. _Proceedings of SIGIR 2009_, 758–759.

#v(0.5em)
Čubranić, D., Murphy, G. C., Singer, J., & Booth, K. S. (2005). Hipikat: A project memory for software development. _IEEE Transactions on Software Engineering, 31_(6), 446–465.

#v(0.5em)
Deng, Y. (2026). Entity-collision: A stratified protocol for attributing retrieval lift in agent memory. _arXiv preprint_ arXiv:2605.29630.

#v(0.5em)
Doyle, J. (1979). A truth maintenance system. _Artificial Intelligence, 12_(3), 231–272.

#v(0.5em)
Druck, G., & Smith, E. (2026). RAG collapse: LLM responses collapse when retrieved documents are self-authored. _arXiv preprint_ arXiv:2608.22118.

#v(0.5em)
Elliott, O. P., & Clark, J. (2024). The impacts of data, ordering, and intrinsic dimensionality on recall in hierarchical navigable small worlds. _arXiv preprint_ arXiv:2405.17813.

#v(0.5em)
Gutiérrez, B. J., Shu, Y., Qi, W., Zhou, S., & Su, Y. (2025). From RAG to memory: Non-parametric continual learning for large language models. _Proceedings of ICML 2025_. arXiv:2502.14802.

#v(0.5em)
Han, Z., Mankikar, M., Michael, J., & Wang, Z. (2025). Search-time data contamination. _arXiv preprint_ arXiv:2508.13180.

#v(0.5em)
Howard, M. W., & Kahana, M. J. (2002). A distributed representation of temporal context. _Journal of Mathematical Psychology, 46_(3), 269–299.

#v(0.5em)
Islam, M. A., Shuvo, M. N. U., Waseem, M., & Abrahamsson, P. (2026). LEDGER: Auditable agent memory under change. _arXiv preprint_ arXiv:2611.20927.

#v(0.5em)
Jiang, D., Li, Y., Li, G., & Li, B. (2026). MAGMA: A multi-graph based agentic memory architecture for AI agents. _arXiv preprint_ arXiv:2601.03236.

#v(0.5em)
Khan, M. N., & Kumar, K. S. S. (2026). Soei: Structured observability of evidence and intent for multi-agent AI software engineering. _arXiv preprint_ arXiv:2610.39468.

#v(0.5em)
Ko, A. J., DeLine, R., & Venolia, G. (2007). Information needs in collocated software development teams. _Proceedings of ICSE 2007_, 344–353.

#v(0.5em)
Kunz, W., & Rittel, H. W. J. (1970). _Issues as elements of information systems_ (Working Paper 131). Institute of Urban and Regional Development, University of California, Berkeley.

#v(0.5em)
LaToza, T. D., & Myers, B. A. (2010). Hard-to-answer questions about code. _Proceedings of PLATEAU 2010_. ACM.

#v(0.5em)
Li, J., & Shi-Nash, A. (2026). MRMS: A multi-resolution memory substrate for long-lived AI agents. _arXiv preprint_ arXiv:2607.04617.

#v(0.5em)
Liu, B., Qiu, H., Goiri, Í., Zhou, J., Fonseca, R., Bianchini, R., Lai, F., & Choukse, E. (2026). Benchmarking long-horizon agent context compaction with CompactionBench. Preprint, Preprints.org, November 2026.

#v(0.5em)
Liu, Y., Fang, J., & Qian, C. (2026). When agent context goes stale: Incoherence in volatile agent context. _arXiv preprint_ arXiv:2610.05281.

#v(0.5em)
Moran, T. P., & Carroll, J. M. (Eds.). (1996). _Design rationale: Concepts, techniques, and use_. Lawrence Erlbaum Associates.

#v(0.5em)
Naur, P. (1985). Programming as theory building. _Microprocessing and Microprogramming, 15_(5), 253–261.

#v(0.5em)
Nygard, M. (2011). Documenting architecture decisions. Cognitect blog, November 15, 2011.

#v(0.5em)
Polyn, S. M., Norman, K. A., & Kahana, M. J. (2009). A context maintenance and retrieval model of organizational processes in free recall. _Psychological Review, 116_(1), 129–156.

#v(0.5em)
Rasmussen, P., Paliychuk, P., Beauvais, T., Ryan, J., & Chalef, D. (2025). Zep: A temporal knowledge graph architecture for agent memory. _arXiv preprint_ arXiv:2501.13956.

#v(0.5em)
Rhodes, B. J., & Maes, P. (2000). Just-in-time information retrieval agents. _IBM Systems Journal, 39_(3–4), 685–704.

#v(0.5em)
Rocchio, J. J. (1971). Relevance feedback in information retrieval. In G. Salton (Ed.), _The SMART Retrieval System: Experiments in Automatic Document Processing_ (pp. 313–323). Prentice-Hall.

#v(0.5em)
Thomson, R., & Lebiere, C. (2026). Rapid prototyping of event-driven contextual memory in the ACT-Up cognitive architecture. _arXiv preprint_ arXiv:2606.28045.

#v(0.5em)
Wang, K., Lin, Y., Lou, J., Zhou, Z., Suvonov, B., & Li, J. (2026). E-mem: Multi-agent based episodic context reconstruction for LLM agent memory. _Proceedings of ICML 2026_, PMLR 306. arXiv:2601.21714.

#v(0.5em)
Wu, R., Zhang, H., Kim, S., & Cheung, S.-C. (2011). ReLink: Recovering links between bugs and changes. _Proceedings of ESEC/FSE 2011_, 15–25.

#v(0.5em)
Xu, H., Zhou, L., Jin, Z., Zhang, X., Tang, B., Li, Z., Zhou, X., & Zhang, J. (2026). MemTrace: State-consistent memory for long-horizon coding agents. _arXiv preprint_ arXiv:2610.04838.

#v(0.5em)
Xu, W., Liang, Z., Mei, K., Gao, H., Tan, J., & Zhang, Y. (2025). A-MEM: Agentic memory for LLM agents. _Proceedings of NeurIPS 2025_. arXiv:2502.12110.

#v(0.5em)
Xue, S., Zhang, Z., Liao, Z., Qin, J., Mu, Y., & Yu, H. (2026). Embeddings vs. grep: Rethinking the dense advantage in code search. _arXiv preprint_ arXiv:2611.20455.

// ==================== Appendices ====================
#pagebreak()
#set text(size: 11pt)
#set par(justify: true)
#counter(heading).update(0)
#set heading(numbering: "A.1")

= T3: protocol, amendments, ablation and limits

This appendix holds the parts of the T3 report that the main text summarizes: the protocol and the two frozen sets, the deviation caught before judging, the contamination found after the results were committed and the scrubbed re-run, the post-hoc component ablation with its table, what T3 leaves standing, and the full statement of limits. The gate results and the pre-committed interpretation are in the main text.

// receipt: docs/plans/saga-t3-preregistration.md:3-4, 6-13, 15-25, 87-99; docs/plans/saga-t3-results.md:3-4, 46-49, 112-120
T3 is the one pre-registered benchmark here whose baseline has a lexical channel, and the walk lost it. On 396 exact receipt-lookup queries, hybrid vector + FTS retrieval placed a receipt conversation in its top 5 more often than the walk (recall\@5 0.813 against 0.581 on the scrubbed snapshot, $p approx 6.9 times 10^(-20)$). On the 27 sealed questions the two did not separate: in the 14-question multi-hop primary stratum the walk answered 3 of 13 judged questions correctly and the hybrid arm 4 of 12 on the scrubbed snapshot (0 R-only against 2 K-only, $p = 0.50$), an underpowered null. The re-litigation tie had motivated the hypothesis that any advantage of the walk lies in multi-hop provenance. T3 tested that hypothesis under a protocol committed before any arm ran (pre-registration commit `2302de0`, committed together with the sealed question roster). The hypothesis was that reinstatement recall beats flat retrieval on provenance questions whose evidence spans corpus sources (plan, conversation, task or artifact), with every arm reading the identical merged corpus. Three arms answered. R is the five-step `csr_why` reinstatement walk with the resolution ledger loaded. K is one-shot hybrid retrieval: a vector top-20 and a full-text (FTS) top-20 fused by reciprocal-rank fusion (score $= sum 1/(60 + "rank")$), deduplicated and truncated to the same budget as R. N is a no-recall arm that sees only the question. K's lexical channel was a fairness grant written into the protocol: commit hashes in chunk text are searchable for K, and only typed edge traversal is exclusive to R. Both retrieval arms render resolution-ledger verdicts through one shared code path.

// receipt: docs/plans/saga-t3-preregistration.md:29-39, 41-53, 58-59, 100-108; docs/plans/saga-t3-final-set.md:7-9; docs/plans/saga-t3-drops.md:3-6, 55-63; docs/plans/saga-paper-draft.md:241; csr-engine/eval-kit/t3/SEAL.sha256; csr-engine/eval-kit/t3/mech_pairs.frozen.csv (receipt_conversations: 181 of 396 rows list more than one); csr-engine/eval-kit/t3/ (four files, no pair builder)
Two sets were frozen. The deep set is 27 questions, kept from 68 git-grounded candidates by four filters (leakage, key verification, multi-hop audit, mechanical dedupe) and a balance pass; each of the 41 drops is logged with its reason. Fourteen questions are multi-hop and nine concern abandonment or supersession; 11 come from this system's own repository and 16 from an unrelated product repository. The question writers were barred from the CSR tools and the reflection database, and every ground-truth key derives from git history, raw plan files and task lifecycles only. The roster is sealed by SHA-256 (`1b331d35…`). The mechanical set is built from commit receipts. Each of 529 commit messages is a query, and its gold is the set of conversations whose transcripts hold the commit hash. Of the 529 hashes, 509 appear in indexed conversation text. A pair is receipt-class (423) when the hash appears inside the tool result of a git invocation, and citation-class (86) when the hash appears only in prose, typically in a later session. A receipt match is not a unique authoring session: the rule applies no creation-only filter, so a later git lookup that prints the hash also matches, and 181 of the 396 scored queries have more than one receipt conversation; a hit is any top-5 result from one of them. Receipt-class gold was the pre-specified primary. The gold conversations of 27 receipt-class pairs fell inside the corpus-freeze exclusion, which leaves 396 pairs. All arms ran against one frozen snapshot (SHA-256 `1d2b9923…`) that excluded the 119 conversations of the benchmark's own construction sessions and held 37 ledger verdicts and 48 plan chunks. The sealed roster, the frozen mechanical pairs and the task inputs are released in `csr-engine/eval-kit/t3/`; the builder that produced the typed pairs (`pairs_typed.csv`) is not.

// receipt: docs/plans/saga-t3-preregistration.md:55-76, 110-118
Gate M scores the mechanical set with no judge: recall\@5 of the receipt-class gold conversation. It passes if R beats K by a sign test over per-query discordant pairs at $p < 0.05$. Gate D scores the deep set. Two cross-family judges (the Codex and Grok command-line agents; no Claude-family judge) grade each answer correct or incorrect against the sealed key, blind to arm, with answers shuffled per question. Gate D passes if R beats K by a sign test over discordant pairs at $p < 0.05$ on the multi-hop subset, the pre-scoped claim; the full set is reported regardless. Both gates were primary, and the interpretation of each outcome was committed in advance.

// receipt: docs/plans/saga-t3-results.md:40-44, 56-74 (Deviations 2-4)
Answers came from one model (Sonnet, through `claude -p`) under an identical prompt scaffold per arm. R and K contexts came from the frozen snapshot; N received none. One deviation was caught before any judging. The first answer batch ran with default settings, and the operator's persistent notes file loaded into all three arms. For questions about this system that file held answer material, and the no-recall arm cited it. The whole batch was deleted and regenerated in a clean-room working directory with settings sources cut and MCP disabled, and a probe confirmed that no hooks, notes or project context loaded. No verdict was taken on the contaminated batch. One Grok grading file was malformed on the first pass; the judge driver re-ran it and all 81 verdicts parsed.

// receipt: docs/plans/saga-t3-results.md:90-110 (Amendment A)
A fourth contamination vector was found after the results were committed. A hook-recursion bug (fixed in PR \#264) had made about 4,800 nested `claude -p` extractor and narrative runs write transcripts that the import watcher indexed as conversations. The frozen snapshot held 4,346 such conversations: 84% of its 5,187 conversations and 56,528 of its 154,389 chunks (36.6% by the receipt's before-and-after chunk counts; the receipt's prose says 38%). The pre-registered freeze exclusion missed them because they predate the cutoff. On the original run they filled 9.5% of R's Gate M top-5 slots against 5.1% of K's, and about 14% of Gate D context items in both arms (7 to 8 of 27 questions per arm). The snapshot was then scrubbed of the 4,346 conversations that a classifier identified as this residue (rule: the first chunk is an extractor prompt; zero overlap with the mechanical gold), which left 841 conversations and 97,861 chunks with zero residual by that classifier (SHA-256 `5af69d81…`). Gate M was re-run in full. For Gate D, K and R answers were regenerated from scrubbed contexts, and the no-recall answers were reused because they read no retrieved context. Both runs are reported in @tab-t3-gates in the main text (@sec-t3). "As published" is the original snapshot; "clean" is the scrubbed one.

// receipt: docs/plans/saga-t3-results.md:126-151 (Amendment B); code, current tree: csr-engine/src/search/reinstatement.rs:360, 377-382, 419; csr-engine/src/search/rerank.rs:47
_Component ablation (post-hoc, exploratory, not pre-registered)._ On the clean snapshot we split K into its channels. FTS alone is weak (0.162). The vector channel carries most of the result (0.697), and fusion adds about 12 points (0.813). The decisive row is the walk: at 0.581 it scores 12 points below its own seed channel. A post-hoc code trace covers the 103 queries that K placed in its top 5 and the walk did not (the K-only discordant pairs on the clean snapshot, a subset of the walk's misses), and proposes two causes. Both are diagnoses made after the results were known; neither was tested by an intervention. First, echo-aware seed selection (`prefer_non_echo_seeds`) prefers seeds that do not contain the query verbatim. On receipt lookup the chunk that contains the commit message verbatim is the gold, so the second hop would spread from the wrong sessions (63 of the 103: gold absent from the walk's top 10). Second, the query-echo demotion ($minus$0.35) and the scaffold demotion ($minus$0.30) stack on receipt-shaped chunks (40 of the 103: gold at rank 6 to 10). These are the defenses that repair contamination in E3. If the diagnosis holds, they demote restatements of the question on conversational recall and demote the answer on exact-key queries, so contamination defense and receipt lookup compete for the same signal; that argues for testing routing by query class rather than assuming one retrieval policy fits every query.

// receipt: docs/plans/saga-t3-results.md:132-137 (Amendment B table)
#figure(
  table(
    columns: (2.4fr, 1fr),
    stroke: none,
    align: (left, center),
    table.hline(),
    table.header([*Arm (clean snapshot, 396 queries)*], [*recall\@5*]),
    table.hline(),
    [K fused (vector + FTS, reciprocal-rank fusion)], [0.813],
    [K vector-only], [0.697],
    [R (reinstatement walk)], [0.581],
    [K FTS-only], [0.162],
    table.hline(),
  ),
  caption: [Gate M component ablation, run after the gates were known. Post-hoc and exploratory; no pre-registered test. The walk falls below the vector channel that seeds it.],
)

// receipt: docs/plans/saga-t3-results.md:76-89 ("What survives, what dies"), 132-139 (Amendment B); docs/plans/saga-t3-preregistration.md:15-25 (arms R, K, N only)
_What survives._ The results file lists what T3 leaves standing. Recall against no recall is not close: 0/27 for the no-recall arm against 13/24 for K as published and 11/22 clean. Exact-key receipts are common: of 529 commits, 423 (80%) are receipt-class, matched by exact key to at least one git tool result that holds the hash. The results file also lists a task-completion audit (T1) and a resolution-ledger replay (T2) among what survives; their primary outputs are not in the repository, so this paper reports no numbers from them. The results file also names exact-key joins as Gate M's winner. Amendment B does not support that reading: K won through its vector channel, and its lexical channel alone scored 0.162. The typed chunk-to-commit join that the receipts motivate was never an arm in T3; it is a design proposal, untested.

// receipt: docs/plans/saga-t3-results.md:40-44, 117, 153-155; docs/plans/saga-t3-preregistration.md:36-39, 62-66; docs/plans/saga-t3-final-set.md:7
_Limits._ The corpus is one operator's recorded history, drawn from two of that operator's repositories. All answers came from one model family, so Gate D measures what retrieved context does for that family. There were two judges, so a split could not be settled by majority; the splits were dropped (6 of 81 as published, 7 clean), and the arms are scored over different denominators. Gate D contexts render each retrieved item as a 200-character excerpt, so multi-hop evidence chains are often cut for both arms; this bounds absolute Gate D scores independently of the R/K comparison. Gate M gold is built from commit hashes present in transcripts, and each query is the commit message, so the construction favours an arm with a lexical channel. The component ablation and its failure trace are post-hoc and exploratory. With two to four discordant pairs per comparison, the Gate D sign test cannot reach significance; the result is a null, not evidence that the arms are equivalent. The full-set null applies to complete-case pairs only: it excludes the seven split arm outcomes of the clean run, so it is sensitive to how those splits would be adjudicated, and no sensitivity analysis was run.

= Field measurements: full table, per-probe caveats and controls

The main text gives the scope of the field probes and the results that bear most on this paper's claims. This appendix gives every probe with its n, interval, status and receipt key, then the caveats that must travel with each number, then the evaluation controls the field work adds.

== What the probes found

#[
#show figure: set block(breakable: true)
#set text(size: 9pt)
#figure(
  table(
    columns: (1.1fr, 1.7fr, 2.6fr, 1fr, 0.95fr),
    stroke: none,
    align: (left, left, left, left, left),
    table.hline(),
    table.header([*Probe*], [*Measure*], [*Result (n, interval)*], [*Status*], [*Receipt key*]),
    table.hline(),
    table.cell(colspan: 5)[_Injection at prompt time_],
    // BLOCK K03
    // receipt: agent-log-testbed/2026-11-16-verdict-review/angles/RESULT.md:86, 191-209
    [Pushed context, next-turn identifier overlap], [Shown chunks with 2 or more distinctive identifiers in the agent's next turn, vs next-ranked unshown chunks of the same project, reconstructed by cosine; held-out SCORE half, human-driven sessions; exposure observational], [21/1,502 (1.40%) vs 22/1,502 (1.46%); difference $minus$0.07 pp, 95% CI $minus$0.88 to +0.68; 74 sessions], [null (registered)], [R-angles],
    // BLOCK K05
    // receipt: agent-log-testbed/2026-11-17 block-audit folder/END-OF-NIGHT.md:92-101; novelty/RESULT.md:5; blocks/RESULT.md:6
    [Prompt-time send gate], [Useful share of sent blocks, replay of candidate build `d953e184` on human-typed prompts; bar 55%], [74/171 (43.3%), Wilson 95% CI 36.1–50.8%. Kind rule plus "not injected earlier in the session": 5/7 on a fresh window (exploratory, not shipped). Machine-written prompts, earlier builds: 0/24 useful (separate sample)], [negative (registered bar)], [R-blocks],
    table.hline(stroke: 0.5pt + rgb("#cccccc")),
    table.cell(colspan: 5)[_After a compaction_],
    // BLOCK K11
    // receipt: agent-log-testbed/2026-11-16-verdict-review/temporal/RESULT.md:33, 79-101, 113-115
    [Source of late-needed detail], [Pre-compaction identifier that reappears 20 or more records later; the receipt's 717 eligible, deduplicated boundaries (each replacing at least 20 turns), 162 sessions, 2026-09-12 to 2026-11-16, not filtered by entrypoint], [2,208 events: host file re-attach 769 (35%), agent Read, Bash or grep 1,140 (52%), recorded history at most 6.5%, no visible source 6.0%. 7/20 sampled details session-specific], [descriptive], [R-temporal],
    // receipt: agent-log-testbed/2026-11-16-verdict-review/validity/RESULT.md:150-168
    [Re-find cost], [Tool calls to re-use a path fact the summary dropped vs kept; 25 human-driven sessions], [Ratio 1.26, session-bootstrap 95% CI 1.00–1.92 (183 vs 43 facts). Re-run after a detector fix (post-hoc): 0.83, CI 0.35–2.57], [inconclusive (registered)], [R-validity],
    // BLOCK K09
    // receipt: claude-self-reflect/.testbed/compact-history/report.md:73-74
    [Recovery tool calls], [Calls in the window from an attributed compaction to the next; before the explicit recovery pointer], [`csr_transcript` in 1/303 windows; general recall in 49; raw transcript opened by Read or Bash in 103 (includes analysis work)], [descriptive], [R-compact-history],
    // BLOCK K08
    // receipt: claude-self-reflect/.testbed/compact-history/report.md:2-4, 9, 39-40
    [Plugin vs host compaction], [Timing over 303 attributed main-thread compactions, 2026-10-05 to 2026-11-05, not filtered by entrypoint; keep-text quality in a separate paired run], [Mid-turn: 0/178 plugin vs 103/124 host. Keep text: +0.39 points, 95% CI $minus$2.92 to +3.95, 16 pairs], [descriptive (timing); null (quality); trigger policy untested], [R-compact-history],
    // BLOCK K10
    // receipt: agent-log-testbed/2026-11-16-verdict-review/survival/RESULT.md:6, 21-22, 34, 44, 48, 94; tables_prereg_run1.md:16, 30, 141
    [Directive presence across compactions], [Owner directive still present in the summary after 1, 2, 3 compactions; human-driven sessions], [99% (n = 474), 50% (n = 389), then 37% loose (95% CI 29–45) or 22% strict (15–31) at the third (n = 340, 59 sessions). Registered rules: 31% and 18%; moved by a post-hoc amendment], [soft lead], [R-survival],
    // BLOCK K12
    // receipt: claude-self-reflect/.testbed/compact-history/analysis.txt:1-4; report.md:16
    [Context floor after compaction], [Median tokens: summary vs first request after the boundary], [Host 25,280 vs 75,109 (n = 125, 26 sessions); plugin 23,222 vs 74,215 (n = 178, 56 sessions). First-request mean difference CI $minus$3,832 to +3,725], [descriptive], [R-compact-history],
    // BLOCK K13
    // receipt: claude-self-reflect/.testbed/compact-audit/report.md:5, 11, 14, 23, 55-65
    [Host compaction metadata], [How host 2.1.318 records a plugin-driven compaction; fleet audit of 2026-11-04, main and subagent transcripts], [Recorded as manual; no record of the compaction model; host off-switches do not stop it. Of 331 boundaries: 143 matched to a plugin record (same session, within 6 s), 179 host auto, 3 typed, 6 manual left unattributed], [descriptive (host-version specific)], [R-compact-audit],
    // BLOCK K15
    // receipt: agent-log-testbed/2026-11-16-verdict-review/validity/RESULT.md:5-9, 69-95; CHANGELOG.md:34
    [Shipped compact receipt], [Was it an arm in any study?], [No. A stand-in built for the study was compared with plain search over the kept record; the endpoint measured was reading cost, and the stand-in arm read 19.5 and 8.3 times as many characters on two slices. The shipped receipt fails open and is unmeasured], [descriptive], [R-validity, R-changelog],
    // BLOCK K07
    // receipt: claude-self-reflect/.testbed/compact-history/report.md:70-71
    [Recovery after host deletion], [Read-back of a session whose host transcript was deleted], [1 session, command-line path only; MCP path not run. Mirror on 2026-11-05: 20,935 files, 19,737 with the source gone], [descriptive (n = 1)], [R-compact-history],
    // BLOCK K50
    // receipt: claude-self-reflect/.testbed/retire/carried.out:1-2; carried.py:1-40
    [Carried-thread retirement], [Carried threads a model names as finished, with a quote from the latest compaction summary; offline replay under a copy of the prompt adapted to carried threads, 2026-11-06], [0 flagged of 24 carried threads in 7 sessions. Zero flagged is not zero wrong; live eligibility, naming, rejection and retirement rates unknown], [not shown to work], [R-retire],
    table.hline(stroke: 0.5pt + rgb("#cccccc")),
    table.cell(colspan: 5)[_Retrieval in use_],
    // BLOCK K20
    // receipt: agent-log-testbed/2026-11-16-verdict-review/astgraph/RESULT.md:3, 31-42, 141, 168-176; claude-self-reflect/.testbed/call-study/codex/REPORT.md:3, 22
    [Code-history demand vs git], [Lookups in human-driven code-editing sessions; hand-read sufficiency; calls to `csr_why`], [Lookups in 75/134 sessions (23.3 per 100 edits). Git answered 19/20 sampled lookups (1 unclear). Git held the reason for 19/20 re-edited symbols. `csr_why`: 9 of 605 calls in a six-week extract], [negative for a symbol graph (registered: weak)], [R-astgraph, R-call-study],
    // BLOCK K25
    // receipt: agent-log-testbed/2026-11-16-verdict-review/temporal2/RESULT.md:8; claude-self-reflect/.testbed/call-study/codex/REPORT.md:69-73
    [Query forms, bank vs field], [Forms that name a file or symbol; question vs keyword shape], [Bank: 0/200 forms. Field sample A: 12/703 searches (1.7%). Field sample B: 276/281 keyword-like, 5 question-shaped], [descriptive], [R-temporal2, R-call-study],
    // BLOCK K26
    // receipt: agent-log-testbed/2026-11-16-verdict-review/temporal2/RESULT.md:39-55
    [Wording sensitivity], [Shipped engine, recall\@5 of the gold conversation, one frozen bank], [Verbatim 1.00 vs paraphrase 0.22; 50 forms each], [descriptive], [R-temporal2],
    // BLOCK K27
    // receipt: claude-self-reflect/.plans/10.1-core-rewrite/adjudicator-log.md:503-504
    [Confidence labels], [Rank-1 results that fully answer, by emitted label; AUC for a fully answering rank-1; model-made labels], [High 8/14 (57%); good 52% of 25; partial 39% of 255; weak 25% of 12. AUC: margin 0.572, top logit 0.677. 306 queries, 1,661 items; kappa 0.813 on a 167-item overlap], [descriptive (calibration untested)], [R-adjudicator],
    // BLOCK K24
    // receipt: claude-self-reflect/.testbed/mcp-empty-probe/commit-msg.txt:3-9; claude-self-reflect/.testbed/call-study/codex/REPORT.md:7, 136
    [Empty or missing results], [Calls that returned no content; not filtered by entrypoint], [44 empty-output calls across 144 transcripts, one protocol mismatch (draft commit message written at the time; counting output not kept). Call extract: 30 no-output wrappers in 342 searches; 0 genuine empty lists], [descriptive (sample-specific)], [R-empty-probe, R-call-study],
    // BLOCK K18
    // receipt: agent-log-testbed/2026-11-16-verdict-review/temporal/RESULT.md:5-18, 148-166
    [Time and cross-store priors], [MRR change on real late needs (test split); not filtered by entrypoint], [Each single prior within 0.03 either way; 330 needs, 53 sessions. Two combined lifts (+0.209, n = 20; +0.124) rode on the edit record; non-edit gold $minus$0.018, 95% CI $minus$0.044 to +0.005, n = 273 (post-hoc)], [null (single priors); combined readout overridden post-hoc], [R-temporal],
    table.hline(stroke: 0.5pt + rgb("#cccccc")),
    table.cell(colspan: 5)[_Verdict reach and instrument coverage_],
    // BLOCK K02
    // receipt: agent-log-testbed/2026-11-16-verdict-review/temporal/RESULT.md:29; temporal2/RESULT.md:16-20
    [Witness verdict reach], [Chunks with a witness binding (`witness_chunk_bindings`); a join to widen it], [160,067 verdicts; 1,710 of 1,052,345 chunks with a witness binding (0.16%, 2026-11-16 snapshot). Ledger rows with a session id: 0. Path-plus-time join: 853 of 648,712 conversation and subagent chunks (0.13%, nightly copy); 14/20 sampled links a real write], [descriptive], [R-temporal, R-temporal2],
    // receipt: agent-log-testbed/2026-11-16-verdict-review/temporal2/RESULT.md:8, 16-20
    [Instrument coverage], [What the store and the question bank let these probes see; nightly copy of 2026-11-16; frozen bank built 2026-11-10], [Witness-ledger rows with a session identifier: 0. Heuristic path-plus-time join: 853 of 648,712 conversation and subagent chunks. Bank forms that explicitly name a file or symbol: 0 of 200. Thinking text and episode rows: availability depends on the snapshot and the adapter, not quantified here], [descriptive], [R-temporal2],
    table.hline(stroke: 0.5pt + rgb("#cccccc")),
    table.cell(colspan: 5)[_Ingestion audits_],
    // BLOCK K31
    // receipt: claude-self-reflect/.plans/release-10.1/READINESS-10.1.md:67-74; scrub-overmatch/gone.json (552 lines), rewritten.json (7,300 lines)
    [Marker scrub over-match], [Chunks a marker-matching scrub dropped, one run on a clone], [552 of 733,388 dropped; at least 322 of them real content that quoted a marker (author-classified lower bound); 7,300 more changed, 233,686 characters cut], [negative (incident)], [R-scrub],
    // BLOCK K34
    // receipt: claude-self-reflect/.plans/release-10.1/bash-edit-recall-2026-10-19/recall.md:3, 35; CHANGELOG.md:596-598
    [Shell-made edits], [Editing session found by the file-history tool (`csr_search_by_file`), live store, 14 days to 2026-10-19, all sessions; repair checked against git on a store copy], [Before the fix, shell only: 1/574 (0.2%, 95% CI 0.0–1.0; the one hit a false positive). Edit tools: 390/408 (95.6%, CI 93.1–97.2). Repair: 0 to 131 of 189 git-identifiable pairs (changelog statement, no results file)], [negative (before the fix)], [R-bash-edits, R-changelog],
    // BLOCK K32
    // receipt: agent-log-testbed/2026-11-16-verdict-review/angles/RESULT.md:21-25
    [Corpus population], [Mirror files by recorded entrypoint, 2026-11-16], [19,642 files: 19,268 headless; 272 human-driven by proxy; 102 not assigned], [descriptive], [R-angles],
    // BLOCK K52
    // receipt: agent-log-testbed/2026-11-16-verdict-review/errorfix/RESULT.md:3, 24-41; angles/RESULT.md:237, 249
    [Error-keyed recall], [Earlier fix applies to the later error; 30 sampled repeat pairs, human-driven sessions, one reader; key refined post-hoc], [8/30; registered kill line 9/30. 20/30 keys generic, not errors, or deliberate failures. Median saving over the 8: 1 call by the registered count, 0.5 by the reader's count], [negative (registered)], [R-errorfix, R-angles],
    table.hline(stroke: 0.5pt + rgb("#cccccc")),
    table.cell(colspan: 5)[_Evaluation controls (next subsection)_],
    // BLOCK K21
    // receipt: claude-self-reflect/.plans/10.1-core-rewrite/adjudicator-log.md:486-488
    [Reranker across CPUs], [Rank-1 agreement across an M5 Max with SME and SME2 and an M3 Ultra without; ONNX Runtime 1.28.0, CPU execution provider; two reranker models; 2026-10-27], [Untuned base Ettin ONNX export: 231/324; tuned run 2 (ONNX hash prefix `73b9d13a`): 95/324. Joint masking of SME, SME2, EBF16: 324/324 for both. No FP32 reference], [descriptive], [R-adjudicator],
    // BLOCK K28
    // receipt: claude-self-reflect/.testbed/swap-eval/raw/recall-hnsw-20261026.json:8; regen-soak-results/recall-final-g6.json:8; proc-audit/recall/recall-live-g1-11f7edb0.json:8
    [Index generation recall], [Tie-aware recall\@10, 10,000 queries], [Never-rebuilt index 0.925; table-order generation 0.948; shuffled generation 0.974 (one build each; no receipted build-to-build spread)], [descriptive], [R-recall-gen],
    // BLOCK K29
    // receipt: claude-self-reflect/.plans/10.1-core-rewrite/adjudicator-log.md:440, 444, 448
    [Rank-1 echo, rewritten core], [Echo at rank 1, 310 keyed queries; rewritten core at build `2ed7f1af` vs installed engine, 2026-10-27], [Installed 33 vs rewritten core 19; whole-stack comparison. At this checkpoint evaluation residue still ranked first and provenance filtering of evaluation output was still open], [descriptive (not the shipped filter)], [R-adjudicator],
    // BLOCK K22
    // receipt: claude-self-reflect/.testbed/core-rewrite/f2a/stages-t.md:1-13; claude-self-reflect/.plans/10.1-core-rewrite/adjudicator-log.md:446-448; claude-self-reflect/.plans/release-10.1/player/csr-10.1-facts.md:3, 97
    [Latency boundary], [Engine stage timer over 323 recall calls, rewritten-core build `8516d582`, 2026-10-27; separately, a warm `csr_reflect_on_past` call at limit 5, build after `d106382e`, 2026-10-23], [Stage audit: total p50 712.7 ms; cross-encoder p50 321.6 ms (41.0% of mean time), full-text 28.4%, enrichment 15.3%, index lookup p50 1.6 ms (0.2%). Warm call: p50 1.05 s, p95 1.47 s (facts sheet, timing script not kept)], [descriptive (two builds, two boundaries)], [R-stages, R-facts],
    table.hline(),
  ),
  caption: [Field probes on the operator's store, October–November 2026, on several builds and dates (see the section opening). One operator; the cross-CPU row used two machines. "pp" is percentage points. Registered means a local pre-registration file written before scoring (self-declared; see Method and scope). Receipt keys resolve in the Evidence register; all except R-changelog are operator-local and not released.],
)
]

// BLOCK K03
// receipt: agent-log-testbed/2026-11-16-verdict-review/angles/RESULT.md:86, 94, 193-209
_Injection at prompt time._ The label, next-turn identifier overlap, is a crude lexical proxy: at least two distinctive identifiers of a shown chunk appear in the agent's next turn and in neither the prompt nor the previous three turns. Exposure was observational: the system chose what to show, and no session was assigned to an arm. A first round found a lift of +1.27 pp (95% CI +0.64 to +2.00) over chunks from other sessions. The registered follow-up used the closest control the logs allow. Candidates retrieved but not shown are not logged, so the control was reconstructed: the next-ranked unshown chunks of the same project by cosine to the stored query embedding. On the held-out SCORE half, 21 of 1,502 shown items and 22 of 1,502 controls overlapped, a difference of $minus$0.07 pp (95% CI $minus$0.88 to +0.68). The EXPLORE half gave +0.62 pp (95% CI $minus$0.05 to +1.34). The earlier lift is therefore not specific to the shown chunks. Shown items came from the hybrid ranker while controls were rebuilt by cosine, and the label misses paraphrase. A causal estimate needs randomized exposure per session and blinded grading of task use. Proactive retrieval from the current context is older than prompt hooks (Rhodes & Maes, 2000).

// BLOCK K05
// receipt: agent-log-testbed/2026-11-17 block-audit folder/END-OF-NIGHT.md:92-107; blocks/RESULT.md:6, 21; novelty/RESULT.md:5-10
The send-gate figure is a replay of candidate build `d953e184`, installed for a blind review; we have not confirmed that it is identical to the 10.1.0 release build. The useful-or-partial line also missed its bar: 136 of 171 (79.5%) against 85%. A rule found post-hoc (keep one item kind), plus a registered addition (drop items already injected in the session), kept 5 of 7 useful blocks on a fresh window. Seven blocks are too few to rule on, and the rule is not shipped.

// BLOCK K11
// receipt: agent-log-testbed/2026-11-16-verdict-review/temporal/RESULT.md:33, 79-101, 113-115; validity/RESULT.md:154-168, 204
_After a compaction._ The late-need probe counts an identifier the agent wrote in an Edit or Write before a compaction, absent from the summary and the kept messages, that reappears 20 or more records later. Such a need occurs at 32% to 42% (floor to ceiling) of the receipt's 717 eligible boundaries, deduplicated and each replacing at least 20 turns. In 39 of 40 sampled attachment cases, the host re-attached the file one to seven records after the summary. The detector is loose: by one reader, 13 of 20 sampled details were general vocabulary. The registered re-find cost readout is inconclusive; the 0.83 ratio is post-hoc. For facts that may not be on disk, only one subtype could be scored: tool-result figures the agent had not saved and the summary dropped, which had 11 re-uses. Owner instructions could not be scored mechanically, and decisions had no detector, so both questions stay open.

// BLOCK K09
// receipt: claude-self-reflect/.testbed/compact-history/report.md:72-74, 77
The recovery-tool census predates the explicit recovery pointer. At the time, the hook's line after a compaction named general recall and the host's line named the raw transcript path; neither named `csr_transcript`. The pointer was changed afterwards to name it. Use of `csr_transcript` under the new pointer is unmeasured.

// BLOCK K08
// receipt: claude-self-reflect/.testbed/compact-history/report.md:4, 9, 39-40, 45-47, 61
The plugin compactor changes when a compaction happens. Carry quality comes from a different comparison, 16 paired keep-text runs separate from the 303 historical events, so the two are not one study. All host compactions in the historical set ran with this system's hooks on, so a host-only baseline was not tested, and sessions were not assigned at random. The timing counts show when compactions fired. Whether the plugin's trigger policy, compacting at turn ends, gives better work than the host's timing remains untested.

// BLOCK K10
// receipt: agent-log-testbed/2026-11-16-verdict-review/survival/RESULT.md:6-7, 22, 44, 46-52, 94; tables_prereg_run1.md:17, 31; ownerrules/RESULT.md:34
Directive presence is a soft lead. A word rule marks an owner message as a directive, and a token-overlap detector checks whether each later summary carries it. The denominators shrink across generations. The detector is weak: it agreed with a hand reading on 13 of 20 pairs (loose) and 15 of 20 (strict), and the loose detector has a 14% carry-over floor. A post-hoc amendment that removed injected templates moved the registered 31% and 18% to 37% and 22%. Harm is not shown: the registered repeat test found no events, and its post-hoc replacement had a precision of 1 of 20. Chen (2026) tests the downstream side, policy retention and later compliance, in a controlled benchmark. Our data show summary presence only, and absence from the summary is not loss of the rule: in a separate hand-read of 64 standing rules in 20 sessions, 27 (42%) were also pinned in a standing instruction file (a memory note, `CLAUDE.md` or a skill file) written before the boundary.

// BLOCK K12
// receipt: claude-self-reflect/.testbed/compact-history/analysis.txt:1-4
The summary is about a third of what the next request carries. A cost model that takes the summary size as the context after a compaction is off by about a factor of three in tokens. We do not estimate cost. That needs cache pricing, the cost of the summary call and the number of later requests.

// BLOCK K13
// receipt: claude-self-reflect/.testbed/compact-audit/report.md:5, 11, 14, 53-65; claude-self-reflect/.testbed/compact-history/report.md:2; claude-self-reflect/.testbed/compact-history/extract.py:67-73
Host metadata cannot separate these compactions, so both audits attribute a boundary through the plugin's own log. The fleet audit (2026-11-04) took every boundary in main and subagent transcripts, 331 in all, and matched each to a plugin start or compact record of the same session within 6 s, or to a typed /compact in the host's prompt history: 143 matched plugin records, 179 were host auto compactions, 3 were typed, and 6 manual boundaries stayed unattributed. The historical study (main thread only, 2026-10-05 to 2026-11-05, 409 boundaries) used the same 6 s window, counted a boundary the host marks as auto as a host compaction, called a boundary typed when it followed a typed /compact of the same session within 400 s, and left the 78 typed and 28 unattributed boundaries out. Its 303 host or plugin boundaries are the windows behind the compaction rows above; the fleet audit is a separate count. The caveat is specific to host version 2.1.318.

// BLOCK K15
// receipt: agent-log-testbed/2026-11-16-verdict-review/validity/RESULT.md:5-9, 69-95; CHANGELOG.md:34
The shipped compact receipt, labelled a prototype in the changelog, writes a source map only after a verified read-back from the mirror. It fails open: a missing, disabled, busy, damaged, oversized or unrecognized transcript omits the receipt, and the compaction still runs. It was never an arm. The study arm that carried a receipt was a stand-in built for the study: one line per 16 turns with the opening of the request, edited paths, error class names and command names. Its comparator was plain search over the kept record, and the endpoint measured was reading cost: agents given the stand-in read 19.5 and 8.3 times the characters of the plain-search arm on two slices, and crawled the record instead of aiming their reads. Accuracy and plan quality were not measured cleanly. The result says nothing about the shipped receipt, which is built and unmeasured.

// BLOCK K07
// receipt: claude-self-reflect/.testbed/compact-history/report.md:70-71
Recovery after host deletion was shown once. For one session whose host transcript was gone, the command-line transcript tool found its compaction boundaries in the mirror and returned the turns before the first boundary. The share of human-driven sessions among the files whose source is gone is unknown.

// BLOCK K20
// receipt: agent-log-testbed/2026-11-16-verdict-review/astgraph/RESULT.md:3, 12, 34, 42-65, 141, 168-176; claude-self-reflect/.testbed/call-study/codex/REPORT.md:3, 22
_Retrieval in use._ Agents look up code history often, and git answers them. Of 1,567 lookup calls, 1,024 came from subagents, and several sampled ones started from a commit named in the brief. Where the earlier turn was compared with git, git was enough in 12 of 13 rows. The `csr_why` count comes from a different window and population. The registered ruling was "weak". This narrows the case for a symbol graph. It does not show that symbol history lacks value: the samples are small, shell-made edits are not captured (below), and many lookups started from a named commit. Post-hoc, the last three sessions on the file held the earlier turn for 467 of 485 symbol re-edits (96%). That is a containment count, and a file has a median of only 2 earlier sessions at re-edit time, so it does not measure graph utility.

// BLOCK K25
// receipt: agent-log-testbed/2026-11-16-verdict-review/temporal2/RESULT.md:8, 27, 33; claude-self-reflect/.testbed/call-study/codex/REPORT.md:3, 69-73
The frozen recall bank has no form that explicitly names a file or symbol (0 of 200), while 12 of 703 field searches do. The witness route fires only on such a query, so the bank cannot test it. The two field samples stay apart: A is the 703 searches in the mirror, B the 281 searches from other projects (a project-name classification) in the call extract. In B, queries are keyword sketches; this paper's queries are full why-questions. The bank result applies to the frozen recall bank, not to the hand-written suites above.

// BLOCK K26
// receipt: agent-log-testbed/2026-11-16-verdict-review/temporal2/RESULT.md:37, 39-55
On that bank the shipped engine is near ceiling for verbatim forms and low for paraphrases. The scored target is the gold conversation, and the engine's runs keep only the top 5. The result is bank-specific, and it does not isolate which channel causes the gap.

// BLOCK K27
// receipt: claude-self-reflect/.plans/10.1-core-rewrite/adjudicator-log.md:503-504
The confidence labels that recall results carry discriminate weakly. Eleven model workers labelled 1,661 excerpts, the top 5 of 336 searches, blind to rank and score. Only 14 rank-1 results carried the label "high". AUC measures discrimination. Calibration is untested: the answer rate behind each label was read on these 306 queries from one machine, and nothing shows that a label carries the same answer rate on other queries, builds or machines. The labels are model-made, and the searches ran on the laptop CPU with matrix extensions on (see the cross-CPU control below).

// BLOCK K24
// receipt: claude-self-reflect/.testbed/mcp-empty-probe/commit-msg.txt:1-11; claude-self-reflect/.testbed/call-study/codex/REPORT.md:136, 181
Some recall failures are delivery failures. A draft commit message written at the time, whose counting output was not kept, reports that from 2026-10-22 four search tools returned empty results, 44 calls across 144 transcripts, because the host declared a task protocol extension but never asked for the task's result. The fix makes that path opt-in (`CSR_MCP_TASKS=1`). No count after the fix was kept, so we do not claim that it removed the failure. In a seeded 60-call sample, seven of eight results that missed existing text were no-output wrappers. These counts are sample-specific and do not rank failure causes.

// BLOCK K18
// receipt: agent-log-testbed/2026-11-16-verdict-review/temporal/RESULT.md:5-18, 69-73, 148-166
Raw time priors did not help on real late needs. The tested priors were time gap, burst, boundary position, typed prompt and thinking block. Two combined arms met the letter of the registered rule. Post-hoc diagnostics show that both ride on the edit record that defines the gold, because the gap before an assistant record is generation time, which is long for edits. The study author overrode the readout to "not supported", and the override is itself post-hoc. Witness, thinking-text and lost-thread signals covered too few turns to test.

// BLOCK K02
// receipt: agent-log-testbed/2026-11-16-verdict-review/temporal/RESULT.md:29; temporal2/RESULT.md:16-20, 79
_Verdict reach._ The 0.16% is a binding share in one snapshot, not the share of searches a verdict affects. The ledger records a span stamp at a commit and no session identifier, so it cannot be joined to the turn that wrote the code. The heuristic join by file path and stamp-change window ran on the nightly copy, which held 1,674 native bindings; its 853 chunks are a separate count. In the sampled links, the changed symbol was named in 9 of the 14 real writes. Per-query reach, display and use each need their own denominator; none was measured.

// BLOCK K01
// receipt: claude-self-reflect/.plans/2026-10-08-10x-way-ahead.html (report written at the time; scorer gone)
The audit of the verdict lines actually delivered to live sessions is reported with the staleness benchmark above, with its weaker receipt class stated there.

// BLOCK K31
// receipt: claude-self-reflect/.plans/release-10.1/READINESS-10.1.md:67-74
_Ingestion audits._ A scrub that cut any paragraph containing one of this system's markers removed real content: code listings, tool output and analyses that only quoted a marker. The count of such drops is an author-classified lower bound; a heuristic found about 30 pure injection blocks among the 552. The response was to cut only structural blocks and to label the rest. That rule is built, and its effect on retrieval is unmeasured.

// BLOCK K34
// receipt: claude-self-reflect/.plans/release-10.1/bash-edit-recall-2026-10-19/recall.md:3, 14-15, 20, 35; CHANGELOG.md:596-598; csr-engine/src/storage/queued_prompts.rs:1-14
Edits made through the shell were invisible to the file-history path we tested. The hook recorded only edit-tool writes in `code_evolution`, and `csr_search_by_file` returned one apparent match for 574 shell-only pairs, which the audit treated as a likely false positive, against 390 of 408 edit-tool pairs. This bounds that tool's path, not retrieval as a whole: chunk text named the path in 259 of the 574 shell-only sessions, and other recall paths were not tested. This paper's coverage ground truth is built from the same table, so limitation (b) in @app-limitations has a second side: sessions that edited only through the shell are missing from the gold as well as from file history, so the gold under-counts them. A repair shipped. The changelog states, with no results file, that on a copy of the store, of 189 pairs that git identifies (committed during a session and touched by no edit tool), file history held 0 before the repair and 131 after (receipt R-changelog). A separate blind spot is unquantified: the host stores a prompt typed while the agent is busy only as a queued-prompt attachment, so a conversation's own chunks never contain it; 10.1 indexes such prompts as rows of their own (`csr-engine/src/storage/queued_prompts.rs`), and how many of them the evaluations above could not see was not measured.

// BLOCK K52
// receipt: agent-log-testbed/2026-11-16-verdict-review/errorfix/RESULT.md:3-6, 13-16, 24-41; angles/RESULT.md:233-250
Error-keyed recall failed on the key we tested: a normalized error line, matched to an earlier session of the same project. The key was refined post-hoc, after the registered key proved broken, so even the repeat rate it rests on was capped at weak. Twenty of the 30 sampled keys were generic summary lines, lines that were not errors, or deliberate test failures. Where the fix applied, in 8 pairs, the median saving was 1 tool call under the registered counting rule (every call from the error to the fix, minus one) and 0.5 under the reader's count of failed calls before the fix; neither is near the 3-call bar. One reader labelled every pair. The ruling covers this key, not error-keyed recall in general.

== Controls these results add to the method

The method above requires a frozen corpus snapshot, one shared index build per comparison, and echo defenses. The field work adds four controls.

// BLOCK K21
// receipt: claude-self-reflect/.plans/10.1-core-rewrite/adjudicator-log.md:486-488
_Pin the inference backend._ Two reranker models, the untuned base export (tagged "shipped" in that audit's log) and tuned run 2, gave different rank-1 results on two Apple CPUs: an M5 Max with the SME and SME2 matrix extensions and an M3 Ultra without, under ONNX Runtime 1.28.0 with KleidiAI SME2 kernels on the CPU execution provider. With the extensions on, the machines agreed at rank 1 on 231 of 324 queries for the first model and 95 of 324 for the tuned one. Joint masking made the machines agree on all 324 queries for both. This shows a backend dependency. It does not show which output is right, because no FP32 reference model was on disk, and the joint mask leaves open which single feature matters. A judged packet is valid for the machine and backend that produced it, so each packet should record CPU features and runtime version.

// BLOCK K28
// receipt: claude-self-reflect/.testbed/swap-eval/raw/recall-hnsw-20261026.json:8; regen-soak-results/recall-final-g6.json:8; proc-audit/recall/recall-live-g1-11f7edb0.json:8; csr-engine/src/index_gen/mod.rs:1-7
_Pin the whole retrieval snapshot._ Shipped chunk search serves from an immutable index generation with a hash manifest (`csr-engine/src/index_gen/mod.rs`). "Share one index build" then becomes "share one generation identifier". Insertion order changes HNSW recall, a known effect (Elliott & Clark, 2024). Here a fixed-seed shuffle replaced table order: one table-order generation scored tie-aware recall\@10 0.948 and one shuffled generation 0.974, on 10,000 queries each; these are single builds with no receipted build-to-build spread, so the size of the change is not established. A generation pins only the base index. Rows newer than the generation come from a mutable delta, and the lexical index, assertions and reranker environment must be frozen separately.

// BLOCK K29
// receipt: claude-self-reflect/.plans/10.1-core-rewrite/adjudicator-log.md:440, 444, 448; csr-engine/src/mcp/synthetic.rs:1-28
_Detect the system's own evaluation residue._ The shipped ranker removes from the candidate pool the prose of sessions that ran this system's own benchmarks and probes, and the output those runs print (`csr-engine/src/mcp/synthetic.rs`). It judges a candidate by its session's other chunks and tool calls, not by the candidate's own text. This extends the echo defense to the measurement rig. The table's echo count does not measure this filter. It compares two whole stacks at one checkpoint of the rewrite, build `2ed7f1af` on 2026-10-27, and the log for that checkpoint records evaluation content still ranking and provenance-based filtering of evaluation output still open; the next entry records it as not built. The filter is a detector, not a guarantee. Its effect on echo, its precision, and how much real work it hides in mixed sessions are unmeasured.

// BLOCK K22
// receipt: csr-engine/src/eval/mod.rs:957-1003 (test_search_latency_p95); claude-self-reflect/.testbed/core-rewrite/f2a/stages-t.md:1-13; claude-self-reflect/.plans/10.1-core-rewrite/adjudicator-log.md:446-448; claude-self-reflect/.plans/release-10.1/player/csr-10.1-facts.md:3, 97
_State the timing boundary._ The eval's lookup check (`test_search_latency_p95` in `csr-engine/src/eval/mod.rs`) embeds its queries first and runs one untimed warm pass; each timer then covers one vector index lookup for the top 5 and nothing else. Embedding, full-text search, the reranker, enrichment and the tool transport fall outside it, so it is not a recall time. The stage audit in the table uses a different boundary, an engine timer around each stage of 323 recall calls at rewritten-core build `8516d582` on 2026-10-27, kept as a results file; there the index-lookup stage is 0.2% of mean call time and the cross-encoder stage alone has a p50 of 321.6 ms. The warm-call figures cover a whole `csr_reflect_on_past` call at limit 5 on an earlier build (after `d106382e`, 2026-10-23); they come from a facts sheet whose timing script was not kept, so they are not a controlled receipt. A latency figure should name its boundary, build and receipt class.

= The shipped system (10.1.0): schema, walk and surrounding mechanisms

The main text states that the shipped system is not the evaluated system and names the differences. This appendix gives the detail, from the public source tree. Each mechanism here is built, not evaluated, unless a receipt is named.

*Status of these in 10.1.0.* The sidechain rule is now path-based: a transcript under a session's `subagents/` folder is imported with `source='sidechain'` and a parent conversation, sidechain results fuse under the parent's session key, and a parent result beats its subagent copy in deduplication; there is still no sidechain score weight. A SessionStart recap shipped, but as a deterministic, zero-LLM composition from stored episode and settlement feeds with receipts attached, framed to the agent as history rather than instructions — not as generated narration; a clause with no evidence behind it is dropped rather than invented (project changelog). Usage-weighted decay is built and structurally inert (Discussion, Silently-inert mechanisms). Chunk-sequence reinstatement remains unbuilt. The three traces now map onto considerably more storage than the three columns listed in @sec-saga, with a fourth role, _settlement_ (@tab-saga-tables). One caveat is receipt-backed: on the operator's store the batch-built `episodes` tables were empty in November 2026 (receipt R-temporal); the episode hop in `csr_why` does not use them — it reads the per-session episode reflections written by the Stop hook.

#figure(
  table(
    columns: (auto, 1fr, 1fr),
    align: (left, left, left),
    stroke: 0.5pt,
    inset: 5pt,
    table.header([*Trace*], [*Tables (10.1.0)*], [*Filled by*]),
    [Intent], [`session_registry`, `conversation_entrypoint`, `chunk_provenance` (author, source conversation, supersedes), per-chunk trust floor], [importer; daemon pass over the host's prompt-history file],
    [Deliberation], [`chunks` with `source` in conversation / sidechain / plan / second-vendor rollout; per-session episode reflections], [importer (watcher); Stop hook],
    [Artifact], [`code_evolution`, `code_nodes` / `code_edges`, `code_node_attribution`, `witness_ledger`, `witness_verdicts`, `witness_chunk_bindings`], [PostToolUse hook; AST pass; witness stamper and verdict pass],
    [Settlement], [`resolution_ledger`, `resolution_proposals`, `assertions`], [`csr_resolve` (user-confirmed); Stop hook from the host's task state; witness verdicts],
  ),
  caption: [Where the three traces, and settlement, live in the shipped schema. Table names are from `csr-engine/src/storage/migrations.rs`. Presence of a table is not a claim that a retrieval path uses it.],
) <tab-saga-tables>

*The shipped walk (10.1.0) differs from the measured one.* The defaults above are unchanged in code (`csr-engine/src/search/reinstatement.rs`), but six behaviours were added after the evaluations in this paper, and none has been measured against the graded gold. (i) Hop 1 fetches a wider pool and every hop-1 hit enters the fused pool, because dropping ranks four and below had made the walk a subset of kNN; seeds must clear a 0.30 floor, with one labelled low-confidence fallback seed at 0.20. (ii) When the query names an identifier that resolves in the AST tables, the walk launches _structural seeds_ from the conversations that the symbol's transcript attribution or incident edges name, and these bypass the semantic floor; a resolved symbol with neither launches no structural seed. (iii) Co-edit spread (Step 3) is _skipped_ whenever a symbol resolves, whether or not a structural seed launched. Our ablations give this default no support: within its grid the channel ablation ranks co-edit spread first (with a marginal contribution that is not separable from zero), and the sham-controlled follow-up measured an AST arm as a null, on an older attribution path than the one the shipped route uses (Phase 2 experiments). The shipped route for symbol-naming queries is therefore untested rather than refuted, and the AST rematch listed under Deferred work is the test of the current route. (iv) Co-edit neighbours and episode hops must clear cosine 0.30, and truncation reserves a quota for episode results. (v) The tool wrapper re-walks at up to four times the width until $k$ non-echo items exist, labels and sinks text the system itself wrote, and excludes conversations that ran the system's own experiments. (vi) The ratification score is attached after ranking and truncation as a shadow field and never ranked. The trust floor (`chunks.min_trust`) is not rank-neutral: the Provenance policy reads it for the penalty on low-trust rows that assert a decision and for primacy eligibility, and each result also carries it as a label. `csr_why` does not consult witness verdicts or the resolution ledger; only general recall does.

== Mechanisms around the walk

The walk is one tool among sixteen. The mechanisms below shipped in 10.1.0 and bear on the claims of this paper. Each is described from the public source and is _built, not evaluated_ unless a receipt is named; none is offered as a result.

_Guards against indexing its own output._ The importer structurally cuts the system's own tool calls and their results, by tool identity, and complete hook-injected wrappers. Text that only _matches_ the system's output grammar is labelled rather than removed — it is sunk at search and skipped at injection — and a sanitizer covers derived artifacts such as episode summaries. Each layer increments a status counter, and the grammar detector is regression-tested against the system's own formatters. These guards cover the emitters the system knows about. No replay with the guards on and off has measured their effect on echo occupancy, and the reason the grammar layer only labels is a measured failure of deleting (Field measurements).

_Trust floor._ Each chunk carries the minimum trust tier of the channel history that preceded it in its transcript, computed from how the content arrived and never from its text; a subagent transcript inherits its parent's floor. This is a conservative taint rule. It records how material arrived, not whether it is authentic; its effect on security and on recall is untested.

_Transcript mirror and recovery._ A background pass captures transcripts as they appear and keeps a secret-scrubbed, per-file compressed copy, with explicit handling of sources that have since gone missing. The aim is that a session stays readable after the host deletes its file. What has been shown is one deleted session read back through the command-line path; on 2026-11-05, 19,737 of 20,935 mirrored files had no source left on disk, 19,087 of them main-thread transcripts (receipt R-compact-history). Recovery through the MCP tool, and the mirror's completeness, are unproven. Because the host deletes transcripts, every longitudinal statement about this corpus is a statement about the records that were available to capture; differential loss by age or session type is unquantified.

_Thread candidates and tentative retirement._ The companion pane nominates possibly dropped threads at render time by differencing the owed items of successive compaction summaries; nothing is stored. A thread moves to a labelled, reversible "guessed done" state when a model names it and quotes a newer summary and code finds that quote, after formatting normalisation, in a summary newer than the thread. The predicate establishes that the quote is present. It does not establish that the work is done, and since compaction summaries can distort task state (Chen et al., 2026), candidate discovery, pick-up accuracy and closure accuracy all need an adjudicated test that does not yet exist.

_Two-channel attribution._ Evidence-bearing symbol attribution was once future work. It shipped: introduction evidence for a symbol is shown as two separate channels — transcript session and git commit — with a disagreement flag (a 48-hour heuristic) and an explicit `unattributed` state in place of a file-level fallback. The AST rematch that this attribution was meant to enable has not been run.

== Release-ancestry decay

_Release-ancestry decay is not an active consumer._ A second evidence-grounded consumer was designed: a decay that replaces wall-clock age with release ancestry where linkage exists. In 10.1.0 it is built but off on the default recall path: under the cross-encoder ranking policy the ordinary age, usage and release-ancestry multipliers are disabled for non-demoted candidates (`csr-engine/src/mcp/tools.rs`); date features and narrow tie-breaks remain, and on that default path release ancestry feeds only the prompt-time injection predictor. The exception is the legacy policy: with `CSR_RETRIEVAL_LEGACY=1`, an older, non-demoted candidate with a valid release label still reaches `apply_tad_with_release_ancestry` (`csr-engine/src/search/decay.rs`). We do not claim it as a consumer. The removal shipped as a side effect of the ranking policy rather than as a tested decision.

= Disclosures: the delivered-verdict audit and leads not yet reportable



== The delivered-verdict audit

// BLOCK K01 + K02 (delivery counts). Receipt class: report written at the time, scorer gone.
// receipt: claude-self-reflect/.plans/2026-10-08-10x-way-ahead.html:317, 376-377, 383, 403-404, 409, 414-415, 421, 431, 437, 441-443, 856; fix check: .plans/pr-drafts/pr-engine.md:8 and .plans/dreamer-study/experiments.md:51
_A deployed-surface audit that the benchmark did not predict._ The benchmark above scores stamps at release tags. What a person or an agent actually sees is a verdict line rendered into a session-start recap or a prompt. An audit of one operator's session-start deliveries over the 30 days to 2026-10-08, under the build then installed, covered 374 distinct delivered verdict lines: 223 were judged false, 116 true but naming the wrong commit, and 22 true and naming the right commit; the report records the remaining 13 as could not be checked. Counted by showings, 683 of 1,135 were false. The causes recorded at the time are specific, and release-tag replay exercises none of them: 28,715 of 34,513 "went stale" verdicts concerned code stamped present at that same HEAD under another project name; the cited receipt was the HEAD the verdict pass observed rather than the commit that changed the span; and the pass judged the checked-out branch, so that 105 of 106 reinstated lines were branch switches. The fix that followed keys anchors by file and symbol without the project, names as receipt the first commit after the witness in which the symbol's parsed text changed, and writes no new verdict unless HEAD descends from where the last one was seen. A separate check ran every current conclusion on one copy of the live database through the same git checker: it flagged 1,826 of 3,345 lines false before that change and 2 of 1,214 after, and both post-fix flags were judged true when checked by hand. In the 30-day window the database held 74,389 verdicts, every one written by backfill re-derivation, and deliveries reached sessions 383 times in recaps and 128 times at prompts (511 in all); those are different units and do not form a rate.

These are three populations — release-tag stamps, delivered session-start lines, and conclusions on a database copy — and we claim no paired before-and-after effect on any one of them; in particular, the 2 of 1,214 are checker flags on the database copy, not a re-audit of the 374 delivered lines. The receipt class is also weaker than the benchmark's: a report written at the time survives on the operator's disk, and the scoring scripts do not (receipt R-delivered). We print the audit because its direction is the point. Precision 1.000 on the benchmark population said nothing about the delivered surface, and the measurement that would turn this audit into a result is a blinded, paired replay of identical delivered events before and after the fix, grading verdict truth, cited-commit correctness and abstention separately.

== Leads that are not yet reportable

_Leads that are not yet reportable._ Five observations from the field work are deliberately left out of this paper's results, because each lacks the measurement that would make it a claim; we name them so they are not mistaken for omissions. (1) Whether the _type_ of a statement predicts the usefulness of injected context better than its similarity score: needs fresh unfiltered candidates, independent blinded labels, and a held-out equal-exposure comparison. (2) Whether a session-narration score blended into ranking helps: needs a frozen comparison against the shipped stack on fresh natural queries, with matched cutoffs and a narration-presence sham. (3) Whether a recovery pointer after a compaction changes real resume work: needs matched quiz and resume arms on fresh sources, or recovery of an earlier study's primary receipts. (4) Whether lexical overlap can tell a finished thread from a forgotten one: needs frozen thresholds and independent completed/unfinished labels. (5) Whether literal counts in a prompt example anchor a model's reported counts, and whether a structured correction log is too sparse to score a session: both need a controlled comparison.

= Evidence register

// Operator-local paths are relative to the operator's projects folder. Repository paths are relative to the worktree root.



#[
#show figure: set block(breakable: true)
#set text(size: 9pt)
#figure(
  table(
    columns: (0.9fr, 2.6fr, 1.3fr, 1.2fr),
    stroke: none,
    align: (left, left, left, left),
    table.hline(),
    table.header([*Receipt key*], [*What it is*], [*Class*], [*Used for*]),
    table.hline(),
    [R-angles], [`agent-log-testbed/2026-11-16-verdict-review/angles/RESULT.md` and `PREREG.md`, 2026-11-16: next-turn identifier overlap of pushed context against two controls; mirror population by entrypoint; the post-hoc error key], [operator-local file, not released; checked], [pushed-context identifier overlap, corpus population, error-key provenance],
    [R-temporal], [`agent-log-testbed/2026-11-16-verdict-review/temporal/RESULT.md` and `PREREG.md`, 2026-11-16: store inventory, late needs after a compaction, time priors], [operator-local file, not released; checked], [verdict reach, late-needed detail, time priors; ledger counts and empty episode tables in the System Design section],
    [R-temporal2], [`agent-log-testbed/2026-11-16-verdict-review/temporal2/RESULT.md` and `PREREG.md`, 2026-11-16 (shipped-engine bank runs recorded 2026-11-10): join coverage, frozen recall bank, field searches], [operator-local file, not released; checked], [verdict reach, query forms, wording sensitivity],
    [R-validity], [`agent-log-testbed/2026-11-16-verdict-review/validity/RESULT.md` and `PREREG-refind.md`, 2026-11-16: validity review, re-find cost], [operator-local file, not released; checked], [late-needed detail, compact receipt],
    [R-survival], [`agent-log-testbed/2026-11-16-verdict-review/survival/RESULT.md`, `tables_prereg_run1.md`, `PREREG.md`, 2026-11-16: directive presence across compactions; with `ownerrules/RESULT.md` in the same folder for the pinned share of standing rules], [operator-local file, not released; checked], [directive presence, pinned rules],
    [R-astgraph], [`agent-log-testbed/2026-11-16-verdict-review/astgraph/RESULT.md` and `PREREG.md`, 2026-11-16: code-history demand, git sufficiency], [operator-local file, not released; checked], [code-history demand],
    [R-errorfix], [`agent-log-testbed/2026-11-16-verdict-review/errorfix/RESULT.md` and `PREREG.md`, 2026-11-16: error-keyed hand check], [operator-local file, not released; checked], [error-keyed recall],
    [R-blocks], [The operator's prompt-time block audit folder of 2026-11-17 under `agent-log-testbed/` (`END-OF-NIGHT.md`, `blocks/RESULT.md`, `novelty/RESULT.md` and their `PREREG.md`): prompt-time block audit and send-gate replay of candidate build `d953e184`], [operator-local file, not released; checked], [send gate],
    [R-compact-history], [`claude-self-reflect/.testbed/compact-history/report.md` and `analysis.txt`, 2026-11-05: 409 main-thread compactions from 2026-10-05 to 2026-11-05, of which 303 attributed to the host or the plugin], [operator-local file, not released; checked], [recovery after deletion, plugin vs host compaction, recovery-tool calls, context floor],
    [R-compact-audit], [`claude-self-reflect/.testbed/compact-audit/report.md`: 2026-11-04: host compaction keys and metadata; 331 boundaries, 143 matched to plugin records], [operator-local file, not released; checked], [host metadata],
    [R-adjudicator], [`claude-self-reflect/.plans/10.1-core-rewrite/adjudicator-log.md`, entries 36 to 38 (rewritten-core builds `2ed7f1af` and `8516d582`), 46 and 49, all 2026-10-27 PT: log written at the time over checked worker receipts], [operator-local file, not released; checked], [reranker across CPUs, confidence labels, rank-1 echo checkpoint, stage p50],
    [R-call-study], [`claude-self-reflect/.testbed/call-study/codex/REPORT.md`: 605 calls to this system's tools in a six-week extract ending 2026-11-14], [operator-local file, not released; checked], [code-history demand, empty results, query forms],
    [R-empty-probe], [`claude-self-reflect/.testbed/mcp-empty-probe/commit-msg.txt`: draft commit message for the protocol-mismatch fix, with its call counts from 2026-10-22], [report written at the time, counting output not kept; checked], [empty results],
    [R-stages], [`claude-self-reflect/.testbed/core-rewrite/f2a/stages-t.md`: engine stage timer over 323 recall calls at rewritten-core build `8516d582`, 2026-10-27], [operator-local results file, not released; checked], [latency boundary],
    [R-facts], [`claude-self-reflect/.plans/release-10.1/player/csr-10.1-facts.md`: release facts sheet read on 2026-10-23: warm `csr_reflect_on_past` timing at limit 5, build after `d106382e`], [report written at the time, timing script gone; checked], [latency boundary],
    [R-recall-gen], [`claude-self-reflect/.testbed/swap-eval/raw/recall-hnsw-20261026.json`, `.testbed/regen-soak-results/recall-final-g6.json`, `.testbed/proc-audit/recall/recall-live-g1-11f7edb0.json`: index recall outputs, 2026-10-26 to 2026-11-09], [operator-local file, not released; checked], [index generation recall],
    [R-scrub], [`claude-self-reflect/.plans/release-10.1/READINESS-10.1.md` and `release-10.1/scrub-overmatch/`: scrub run on a clone, 2026-10-13], [operator-local file, not released; checked], [scrub over-match],
    [R-bash-edits], [`claude-self-reflect/.plans/release-10.1/bash-edit-recall-2026-10-19/recall.md` and `results.json`: shell-edit recall in file history, measured 2026-10-19], [operator-local file, not released; checked], [shell-made edits (before the repair)],
    [R-changelog], [`CHANGELOG.md` (public, in the repository): shell-edit repair checked against git, 0 to 131 of 189 pairs; compact-receipt fail-open behaviour], [repository file; changelog statement, no results file; checked], [shell-made edits (repair), compact receipt],
    [R-delivered], [`claude-self-reflect/.plans/2026-10-08-10x-way-ahead.html`, with the later fix check in `.plans/pr-drafts/pr-engine.md` and `.plans/dreamer-study/experiments.md`: audit of delivered verdict lines], [report written at the time, scorer gone; search-located], [delivered-verdict audit (staleness benchmark)],
    [R-retire], [`claude-self-reflect/.testbed/retire/carried.out`: offline replay of carried-thread retirement, 2026-11-06], [operator-local file, not released; checked], [carried-thread replay (field table; Silently-inert mechanisms)],
    [R-samepage], [`claude-self-reflect/.testbed/samepage/repeat/result.txt`: five repeat reads of two sessions], [operator-local file, not released; checked], [repeat-read variation (Limitations)],
    [R-attribution-2], [`claude-self-reflect/.plans/2026-08-01-*-gold-prereg.md` (file name carries a project name, elided), 2026-08-01: on a second project, every code node that change events can attribute (711 of 3,398) was compared, not a sample; a node agrees when its stored `first_conv_id` equals the session that change events name as its earliest introducer (355 of 711, bar 90%). Also a 34-query grid, quarantined after its gold failed an independent audit], [operator-local file, not released; checked], [second-project attribution (E1b)],
    [R-code], [`csr-engine/src/index_gen/mod.rs`, `csr-engine/src/mcp/synthetic.rs`, `csr-engine/src/eval/mod.rs`, `csr-engine/src/storage/queued_prompts.rs`], [repository file; checked], [mechanisms named in the field notes and the controls subsection],
    table.hline(),
  ),
  caption: [Evidence register for the field measurements. Operator-local files derive from private transcripts and are not released, so a reader cannot re-run them. "Report written at the time" means a note made on the day whose underlying script or output is not kept. "Checked" means the file was opened on 2026-11-21 and the numbers this paper prints from it for the uses listed were matched to it. "Search-located" means a receipt search over the evidence folders found the file and the stated numbers, and the file was not re-opened for this register.],
)
]

= Limitations in full <app-limitations>

*(a) Scale and design.* Each experiment has its own small query set — 12 hand-written queries in the spike, 20 graded-gold queries of which 12 carry a mapped origin, and 396 receipt lookups with 27 sealed questions in T3 — and all of them come from one operator's history: no external dataset, no multi-organization coding history. The qualitative criterion M3 rests on the single-annotator spike reading alone; where LLM judges are used in receipted experiments (T3) they are two cross-family judges with split verdicts dropped. Twelve queries remains a small suite. The graded E2/E1 metrics are point estimates without interval estimates on 12 mapped queries, and both arms miss many of the verified origins, so a one-to-two-query swing moves the relative lifts materially. Paired-bootstrap intervals have since been produced for the expansion channels only, on a reduced five-arm re-run against a frozen evaluation unit (E1b): every contrast includes zero, so E1's arm ordering stands as a within-grid ranking rather than a separated effect, and the AST arm's null is additionally bounded by channel sparsity (fired on 8 of 20 queries) and by 50.7% attribution accuracy. The full seven-arm grid remains unbracketed. The behavioral benchmark's rubric grades are author-graded blind-to-arm, not independent human evaluation.

*(b) GT and graph plumbing.* GT is built from `code_evolution`, which Arm B also uses in step 3. Circularity is mitigated by seed dependence, but not eliminated; the spike-era argument from blend-channel dominance no longer holds, because E1 reversed that attribution on graded gold. An attempted independent GT from VCS commit trailers failed on sparse trailer coverage and on self-contamination of the ID bridge (see Method); the mitigation path is specified but not yet realized.

*(c) Observer effect partially mitigated, not solved.* Frozen snapshots and Phase 1.5 echo defenses repair the measured displacement cases but do not rescue origin conversations for decisions whose re-askings dominate entire conversations; conversation-level exclusion and `supersedes` chains remain open work. 10.1.0 adds partial measures — a whole-row question-echo rule, a wrapper that re-walks and sinks echoes, and exclusion of the system's own experiment conversations at the door — none of them measured against the E3 protocol; full conversation-level exclusion is still open.

*(d) Episode-chain value revised, not settled.* The episode hop contributed zero GT hits in the coverage-proxy era (sparse/young `prev_episode_id` chains); on graded gold the episode-only ablation arm is a strong origin-finder (0.321 origin-MRR) but weak on evidence depth. Its value is now positive but characterized on 12 mapped queries only. In 10.1.0 the hop reads the per-session episode reflections written at session end; the separate batch-built episode tables are not wired to the importer and were empty on the operator's store in November 2026.

*(e) Generalizability.* The primary suite is maximally dogfooded (CSR indexing its own development). The coverage result stands on the self-describing primary corpus alone. The receipted experiments that do span a second project of the same operator are E2 and E1 (8 of the 20 queries, 5 of the 12 mapped origins), T3 (16 of its 27 sealed questions) and the attribution check in E1b; neither removes the single-operator confound: all indexed history is one person's work with one agent stack. (10.1.0 also ingests a second vendor's agent rollouts and plan files; the share of the corpus they form was not measured, and it is still one operator.) Multi-operator and multi-organization validation remain open.

*(f) GT layers and their residual weaknesses.* The coverage results use a file-touch proxy (all `code_evolution` sessions touching the target file count equally), which counts design-originating, implementing, and merely-discussing sessions as equally correct and shares plumbing with Arm B's graph channel; those results should be read as coverage of _decision-relevant activity_. The graded gold (E2) removes the proxy but has its own limits: grades derive from LLM-extracted dialog-acts (dual-vendor consensus, directs $kappa = 0.41$ — moderate; splits resolved conservatively; the owner audited only the five injected-origin maps, and the 31 vendor-split items and 12 grade spot-checks went unaudited), the gold is same-corpus and only partly disjoint from retrieval (grades come from dialog-acts, sealed origins and ship ledgers, but the graded pool includes file-touch history from the `code_evolution` table the graph channel reads), and origin-MRR rests on n=12 single-operator sealed recollections.

*(g) Concurrent-work race.* ACT-Up, MRMS, and E-mem have now been close-read in full (Related Work reflects their actual mechanisms and evaluations), but the space is moving quickly; other concurrent preprints may exist that we have not surveyed, and the composition could be assembled by others within months.

*(h) Incomplete three-source modeling.* CMR motivates source-factored reinstatement of intent vs. deliberation vs. artifact (Polyn et al., 2009). Phase 1 labels sidechains and stores sequence but still uses a single fused context blend and does not yet weight by `is_sidechain` or `seq`. In 10.1.0 the sidechain label drives session-key fusion and parent-over-copy deduplication, but it is still not a score weight, and the blend is still single.

*(i) The shipped system is not the evaluated system.* Every retrieval experiment with a repository receipt was run on the July–August build; the only rows that exercise the shipped engine are field observations (Field measurements), and they are labelled as such. The shipped walk adds structural seeds and skips co-edit spread on symbol-naming queries; the shipped general recall path is hybrid and cross-encoded; age decay is off by default. No experiment here compares the shipped walk with the shipped recall path, and the one benchmark with a hybrid baseline (T3) went against the walk.

*(j) Receipts.* This paper reports only results whose receipts are in the record. Reader-verifiable claims are the ones with files under `docs/plans/` and `csr-engine/eval-kit/`. The field measurements have receipts too, but on the operator's disk and unreleased, and they are labelled as such.

*(k) Model readings shown to the person.* The companion pane shows a per-session count of asks that a model judges were missed. It is a model judgment, displayed as a count and never as a percentage. On one long session, five reads with a reduced copy of the prompt returned 2 of 21 three times, 2 of 28 once and 0 of 21 once, while a short session's counts were stable (receipt R-samepage; five reads of a reduced prompt, not the live path). Neither accuracy nor reliability on the live path has been measured, and the counts should not be compared across sessions.

*(l) What the instruments can see.* The witness ledger has no direct join to a session (no row carried a session identifier in the snapshot examined), and the one session-keyed table of span hashes uses different hash inputs, so the two cannot be joined directly; the coverage and wrong-link rate of any bridge are unmeasured. The frozen recall bank has 0 of 200 forms that explicitly name a file or symbol, so it cannot exercise the routes such queries trigger. Whether thinking text or episode rows are available depends on the snapshot and the adapter. Results that depend on any of these are scoped to the measured coverage (receipts R-temporal, R-temporal2).

= Deferred work in full

The three experiments queued as follow-ups — the factored ablation grid, graded provenance gold, and the controlled contamination experiment — have been executed and are reported above (Phase 2 experiments). What they leave open sets the queue. From E1: fusion re-weighting (a larger graph share, per-channel quotas) and a blend-free graph+episode arm. From E2: the origin-finding floor — both arms miss many of the verified origins, and four of the five owner-audited injected-origin maps date from the corpus's earliest era — motivating age-compensating retrieval; and a cross-persona replication with marketing-operations queries against the machine's purpose-built release-train ledger. From E3: conversation-level echo exclusion (chunk-level demotion repairs occupancy but cannot rescue origins whose re-askings dominate whole conversations) and `supersedes` population. From the ratification negative result: node-level act-strength weighting is closed by its pre-registered gate, not deferred; the formulation that remains open is edge-level ratification — acts bound to (conversation, artifact) pairs, weighting reinstatement graph edges and joined query-conditionally — which is untested.

_Honest attribution, and the AST rematch it gates._ The follow-up's second bound (E1b) is a work item, not only a caveat. Symbol→conversation attribution was, at evaluation time, a file-level projection: 50.7% agreement with evidence-bearing change events against a 90% bar, with 499 of 532 files carrying one uniform id. Two independent channels can replace it without inferring anything. Transcript change events cover 33.1% of indexed symbols (2237 of 6767), with a structural ceiling of 44.5% once the kinds that never appear in change events (constants, modules) are excluded — sparse, but exact and fine-grained. Git introducers, recovered as the first line of `git log -L` over each symbol's stored span, corroborate the transcript channel on 82.2% of the overlap within 48 hours (1103 of 1342, median gap 1.3 hours) on the receipt run's clean-span sample — 74.1% (1611 of 2174) on the full live backfilled table, whose denominator includes every dual-channel symbol rather than the sampled clean-span subset; both are reported rather than reconciled — and reached 274 of a 288-symbol sample (95.1%) of the symbols that have no transcript event at all — dense, but coarse: among the 20 most recent introducing commits examined, single squash-merge commits introduced between 1 and 393 symbols, and the repository's analysis note records larger squashes still. (Receipts for this paragraph: the code-graph analysis note in `docs/analysis/` for the agreement, ceiling and corroboration rates; the transcript-coverage count, the median gap, the 288-symbol sample and the squash range rest on operator-local planning files that are not released.) The asymmetry is the design, not a defect: git covers deleted and pre-ledger history, transcripts resolve granularity inside a squash, disagreements are surfaced as two labeled channels rather than silently merged, and symbols with neither channel are rendered `unattributed` rather than inheriting their file's first toucher. Shipping that, and only then re-running the AST arm on it, is the path by which the structural null becomes falsifiable. Status at 10.1.0: the two-channel attribution shipped (System Design); the rematch has not been run; and the shipped walk already routes symbol-naming queries through this layer without that evidence. A November probe also narrows what the rematch can be expected to show: in the sampled sessions git answered most code-history lookups (Field measurements).

Learned decay (Anderson & Schooler, 1991) is theoretically motivated, but as built it is inert (Silently-inert mechanisms) and must be repaired before accumulated logs can matter. Chunk-level temporal reinstatement using `seq` and sidechain-aware scoring remain unbuilt; a SessionStart recap shipped in deterministic form. The comparison that now matters most is the one no experiment here contains: the shipped walk against the shipped hybrid, cross-encoded recall path, by query class. External and multi-operator validation remains the scientific next step after that.

_Leads that are not yet reportable._ Five questions raised by the field work are left out of this paper's results because each lacks the measurement that would make it a claim: whether statement type predicts useful injected context; whether a session-narration blend helps ranking; whether a recovery pointer changes real resume work; whether lexical overlap can detect thread closure; and whether literal counts in a prompt anchor a model-read count. Appendix D names the measurement each needs.

= Further related work

*Further related work.* An obvious ancestor is Hipikat: (Čubranić et al., 2005) built a project record from heterogeneous software artifacts — source, issues, mail, documents — and recommended relevant ones to newcomers. What changes with coding agents is the source of the record (it writes itself as transcripts) and, as this paper measures, the way the record's own growth displaces what is sought. Between August and November 2026 several papers moved into the same space, and they sharpen what this paper can and cannot claim. _On contamination:_ Han et al. (2025) show search-capable agents retrieving benchmark answers from the open web, and Druck and Smith (2026) show, in recursive simulations, responses losing diversity when the retrieved documents are self-authored. Both are neighbours of our finding, which concerns the evaluation dialogue itself entering the index and displacing an independently identified origin. _On staleness:_ Concord (Liu et al., 2026) binds tool observations to their mutable sources and updates, annotates or suppresses stale context; LEDGER (Islam et al., 2026) lets a model propose facts while explicit rules decide each fact's current value; and MemTrace (Xu et al., 2026) keeps file-, symbol- and test-anchored execution records and checks them against the repository before reuse, with gains on downstream coding benchmarks. MemTrace is the closest combined precedent for our witness verdicts and has what ours lacks, an end-task evaluation; what ours adds is narrower — commit-anchored span hashes, abstention as the default, and a published account of where the mechanism does not reach. _On attribution:_ the Agent Trace specification (Agent Trace, 2026) records, at the moment code is written, which ranges came from an agent and from which conversation; our ledger records content at a commit and tries to reconstruct that link afterwards, which is the harder and weaker position. _On compaction:_ Chen (2026) measures safety constraints lost to context compaction in a controlled benchmark, and CompactionBench (Liu et al., 2026) and CompAct-Bench (Chen et al., 2026) evaluate compaction by whether the task still completes, the latter reporting that compressed context can distort task state rather than merely drop it. _On evaluation of use:_ CounterMem (Baklanov et al., 2026) tests whether a system's answers change when only the retrieved context changes, and Soei (Khan & Kumar, 2026) separates an agent's completion claims from proof across real coding sessions. Finally, Xue et al. (2026) find that the dense-retrieval advantage over lexical search in code search depends on evaluation choices and on queries that lack rare identifiers. T3 is a neighbouring observation rather than the same one: there a fused dense-plus-lexical baseline beat the walk, while the lexical channel alone was weak.
