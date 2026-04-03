# CSR Memory Retrieval Algorithm Research

## Three Documents in This Directory

### 1. **CSR_NOVEL_ALGORITHMS.md** (30 KB, Full Research)
Comprehensive analysis of CSR's novel algorithmic contributions vs claude-mem.

**Key Sections:**
- Part 1: Critique of "Predictive Contextual Injection" (PCI) — identifies 5 key weaknesses
- Part 2: Three Novel Algorithms
  - OBRL (Outcome-Biased Reinforcement Learning) — learn from usage feedback
  - CFP (Conversation Flow Prediction) — anticipate next questions
  - HMC (Hierarchical Memory Consolidation) — merge redundant memories
- Part 3: Improved Decay Formula — type-aware, event-driven (beats exponential)
- Part 4: Template Story Generator — 90%+ coverage, zero API cost
- Part 5: Competitive Positioning vs claude-mem (50k GitHub stars)
- Part 6: Implementation Roadmap (26 weeks, 6 phases)
- Part 7: Metrics to Prove Superiority

**Read this if:** You need the full context, academic grounding, and competitive analysis.

---

### 2. **IMPLEMENTATION_SUMMARY.md** (10 KB, Executive Summary for Codex)
Condensed version focused on what Codex needs to review.

**Key Sections:**
- OBRL, CFP, HMC with brief Rust sketches
- Validation strategy for each algorithm
- Type-aware decay with examples
- Template story generator algorithm
- 6-month implementation roadmap
- Codex review checklist (12 items)
- Success metrics (post-implementation)

**Read this if:** You're Codex evaluating feasibility, or a developer starting implementation.

---

### 3. **RUST_IMPLEMENTATION_PATTERNS.md** (8 KB, Code-Level Details)
SQL schemas, Rust function signatures, and test patterns.

**Key Sections:**
- OBRL: Database schema + `record_injection()`, `record_outcome()`, `compute_hook_reward_profile()`
- CFP: `FlowSignature`, `QuestionType`, `predict_next_questions()`, `record_flow_signature()`
- HMC: Agglomerative clustering with `check_contradiction()`
- Type-aware decay: Enum-based `adaptive_decay()` function
- Integration points in SessionStart hook and daemon
- Testing patterns (6 test examples)
- Codex evaluation checklist

**Read this if:** You're implementing the algorithms or reviewing code quality.

---

## Quick Navigation

| Question | Document | Section |
|----------|----------|---------|
| "What makes CSR different from claude-mem?" | CSR_NOVEL_ALGORITHMS | Part 5 (Competitive Positioning) |
| "Is this actually implementable?" | IMPLEMENTATION_SUMMARY | Roadmap + Codex Review Checklist |
| "Show me the Rust code outline" | RUST_IMPLEMENTATION_PATTERNS | All sections |
| "What's the academic novelty?" | CSR_NOVEL_ALGORITHMS | Part 2 (Three Novel Algorithms) |
| "How do you validate these ideas?" | IMPLEMENTATION_SUMMARY | Validation Strategy |
| "What are the database schema changes?" | RUST_IMPLEMENTATION_PATTERNS | Section 1-3 (SQL CREATE TABLE) |
| "What are the publication venues?" | IMPLEMENTATION_SUMMARY | Publication Venues (3 papers) |

---

## Executive Summary (TL;DR)

### The Problem
CSR uses a simple "Predictive Contextual Injection" (PCI) scoring model:
```
final_score = w_sem * semantic_sim + w_rec * recency + w_file * file_overlap + w_err * error_match + w_phase * phase_boost
```

But PCI has **5 critical weaknesses**:
1. **No feedback loop** — never learns which retrievals Claude actually uses
2. **Static lifecycle weighting** — same w_phase for all sessions
3. **No memory relationships** — treats 10 Docker memories as independent, not clustered
4. **Exponential decay** — all memories fade equally (wrong for security facts)
5. **No negative signals** — can't detect failed approaches

### The Solution: Three Novel Algorithms

#### 1. OBRL (Outcome-Biased Reinforcement Learning)
**What it does:** Track which injected memories Claude uses, learn hook-specific weights.

**Why it matters:** 5-10% improvement in session completion rate (measurable, no API cost)

```rust
// Instead of fixed w_phase, learn it dynamically:
w_hook[SessionStart] = 1.2   // High success rate
w_hook[PreCompact] = 0.8     // Lower success rate
```

#### 2. CFP (Conversation Flow Prediction)
**What it does:** Anticipate the next 2-3 questions, inject context proactively.

**Why it matters:** Reduces back-and-forth loops (proactive vs reactive)

```
Past pattern: Debugging → Hypothesis → Solution → Verification
Current: User asks "Debug" question
Prediction: Pre-load "Solution" + "Verification" context
```

#### 3. HMC (Hierarchical Memory Consolidation)
**What it does:** Merge 10 redundant memories into 3 clusters with contradiction flags.

**Why it matters:** 70% reduction in redundant context while preserving signal

```
Input: 10 Docker memories (5 networking, 3 mounting, 2 builds)
Output: 3 clusters (each with primary memory + contradiction flag)
Benefit: Cleaner injection, users know if solutions conflict
```

### Additional Improvements

#### Type-Aware Decay (vs Exponential)
- Security vulnerabilities: DON'T decay (permanent facts)
- Reused solutions: boost (confirmation signal)
- Deprecated solutions: rapid decay (new approaches prioritized)
- Recurring errors: boost (pattern recognition)

#### Template Story Generator (90%+ Coverage, Zero Cost)
- Convert V3 extraction → 2-3 sentence summaries
- No LLM call ($0 vs $0.012)
- 95% coverage for success, 90% for abandoned, 80% for long sessions

### Competitive Positioning

| Feature | claude-mem | CSR Current | CSR Proposed |
|---------|-----------|-------------|---|
| Feedback loop | ✗ | ✗ | ✅ OBRL |
| Anticipation | ✗ | ✗ | ✅ CFP |
| Redundancy handling | ✗ | ✗ | ✅ HMC |
| Type-aware decay | ✗ | ✗ | ✅ Adaptive |
| Story cost | $0.012 | $0.012 | $0 (template) |
| **GitHub stars** | **50k** | TBD | **100k+ target** |

### Validation Plan

1. **OBRL**: 4 weeks, 50 projects → measure 5-10% improvement in session completion rate
2. **CFP**: Train on 200 projects, test on 50 → achieve >70% accuracy on next-question prediction
3. **HMC**: 1000 Docker conversations → reduce redundancy from 30% to 5%
4. **Type-aware decay**: A/B test old vs new formula

### Implementation Roadmap

- Week 1-4: OBRL Foundation (storage layer)
- Week 5-10: CFP (flow prediction + classifier)
- Week 11-18: HMC (clustering with contradiction detection)
- Week 19-22: Type-aware decay
- Week 23-26: Story generator + documentation

**Total: 26 weeks, 3 publications (OBRL, CFP, HMC), publishable/defensible/implementable**

---

## For Different Audiences

### For Your Boss / Product Manager
CSR will beat claude-mem because it:
1. Learns from usage (feedback loop others don't have)
2. Anticipates user needs (proactive, not reactive)
3. Reduces noise by 70% (cleaner injections)
4. Saves $0.012 per story with template generator (cost advantage)
5. Is publishable (academic credibility)

**Expected outcome:** 50k → 100k GitHub stars within 6 months

### For Codex (Code Evaluator)
Focus on:
- [ ] Race conditions in OBRL's concurrent outcome tracking
- [ ] Performance of HMC's O(n²) clustering on 10k memories
- [ ] Robustness of CFP's question classifier (is heuristic good enough?)
- [ ] Contradiction detection (too brittle? Need ML-based approach?)
- [ ] DB migration strategy (backward compatibility)
- [ ] Integration: do OBRL + CFP + HMC work together?

See IMPLEMENTATION_SUMMARY.md for full checklist.

### For ML Researchers
Three papers worth submitting:
1. **OBRL**: "Outcome-Biased RL for Memory Ranking in LLM Agents" (MLSys, ICLR workshop)
2. **CFP**: "Predicting Next Questions in Multi-Turn Conversations" (ACL, EMNLP)
3. **HMC**: "Hierarchical Memory Consolidation for Noise Reduction in RAG" (SIGIR, RecSys)

Each includes:
- Formal algorithm definition
- Validation on public benchmarks
- Comparison to baselines (exponential decay, cosine similarity)
- Open-source implementation (CSR)

---

## Key Metrics (Post-Implementation)

| Metric | Target |
|--------|--------|
| Session completion rate | 65% → 72% (+5-10%) |
| CFP next-question accuracy | >70% |
| HMC redundancy reduction | 30% → 5% (6x better) |
| Search latency | <50ms total |
| Cost per story | $0.012 → $0 |
| GitHub stars | 50k → 100k+ |

---

## Why This Wins

CSR's moat is **not math** — it's **practical innovation**:

1. **Novel**: OBRL is the first memory system with learned feedback from usage
2. **Proactive**: CFP anticipates needs (claude-mem is reactive)
3. **Pragmatic**: HMC uses simple agglomerative clustering (not complex ML)
4. **Defensible**: Type-aware decay recognizes memory types matter
5. **Profitable**: Template stories save $0.012 per conversation

This is **publishable** (3 venues), **defensible** (novel algorithms), and **implementable** (Rust, 273 tests, 6 months).

---

## Next Steps

1. **Codex Review** (this document + IMPLEMENTATION_SUMMARY.md)
   - Identify architectural issues
   - Flag risky assumptions (heuristics, performance)
   - Suggest DB migration strategy

2. **Start Phase 1** (OBRL Foundation, 4 weeks)
   - Add `memory_injections` + `memory_outcomes` tables
   - Implement `record_injection()` and `record_outcome()`
   - Start collecting data

3. **Validate CFP** (4 weeks before Phase 2)
   - Test question classifier on 100 real conversations
   - Measure prediction accuracy (need >70%)
   - Adjust heuristic if needed

4. **Plan publication** (parallel with Phase 5)
   - Draft papers for MLSys, ACL, SIGIR
   - Benchmark against baselines
   - Open-source implementation

---

## Files in This Research

```
docs/research/
├── README.md                          (this file)
├── CSR_NOVEL_ALGORITHMS.md            (30 KB, full analysis)
├── IMPLEMENTATION_SUMMARY.md          (10 KB, exec summary for Codex)
└── RUST_IMPLEMENTATION_PATTERNS.md    (8 KB, code details)
```

**Total**: 48 KB research, 4 detailed sections, ready for Codex evaluation or implementation.

