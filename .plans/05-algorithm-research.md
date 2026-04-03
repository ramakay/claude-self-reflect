# CSR Algorithm Research: Beating claude-mem & mem0

## Date: 2026-04-03
## Sources: Codex evaluator, Gemini researcher, direct DB analysis

---

## Competitive Landscape

### mem0 (51.9k stars) — github.com/mem0ai/mem0
- **Architecture**: Hybrid (Vector + Graph + KV). LLM extracts discrete facts.
- **Retrieval**: Semantic + Neo4j graph + reranking + metadata filters
- **Weakness**: "Junk memory" accumulation, summarization loss, memory/thread leaks
- **Embedding**: OpenAI/HuggingFace, supports Qdrant/Chroma/Pinecone

### claude-mem (~50k stars) — github.com/thedotmack/claude-mem (v10.6.3)
- **Architecture**: SQLite FTS5 + ChromaDB, 6 hooks, background AI compression via Agent SDK
- **Retrieval**: 3-step progressive disclosure (search→timeline→observations, ~10x token savings)
- **Hybrid search**: FTS5 rank + Chroma cosine similarity + access frequency boost
- **Embedding**: `all-MiniLM-L6-v2` (384d) — same model as CSR's FastEmbed
- **Dedup threshold**: cosine similarity 0.75
- **CRITICAL GAP**: **NO DECAY ALGORITHM** — append-only, community requested -0.05/day, unimplemented
- **Active bugs**: #1587 worker daemon hangs on Linux, #1565 stale 3-day-old previews, #1566 ChromaSync bugs
- **Weakness**: SQLite fragmentation/locking, "memory rot" (3,400+ observations bloats vector space), embedding code/logs noise
- **Philosophy**: "Observer AI" — real-time background note-taker
- **Full analysis**: `docs/analysis/claude-mem-competitive-analysis.md`

### CSR (Claude Self-Reflect)
- **Architecture**: Rust engine (93ms cold start), SQLite + in-process HNSW, 6 hooks
- **Retrieval**: Multi-signal scored (semantic×0.5 + recency×0.2 + file_overlap×0.2 + error×0.1)
- **Embedding**: FastEmbed BGE-small (384d), local, 2.5ms/text
- **Enrichment**: 3-layer (heuristic→V3 extraction→Haiku narrative)
- **Philosophy**: "Conversational archive" — proactive lifecycle-aware injection

### Claude Code built-in
- **Architecture**: File-based MEMORY.md, always loaded, 200 line limit
- **No selective retrieval**, no embeddings, manual curation

---

## Key Differentiators (What CSR Has That Others Don't)

| Feature | mem0 | claude-mem | CSR |
|---------|------|-----------|-----|
| Lifecycle-aware retrieval | No | No | **YES** (phase-specific weights) |
| In-process HNSW (no external DB) | No | No | **YES** (Rust, 0.8ms search) |
| Real-time chunking (same-session search) | No | Partial | **YES** (prompt-submit hook) |
| 3-layer progressive enrichment | No | AI compression | **YES** (heuristic→V3→Haiku) |
| Anti-pattern injection | No | No | **YES** (past failures surfaced first) |
| Proactive injection (no asking) | No | Yes | **YES** |
| Rust performance | No (Python) | No (Node/Python) | **YES** (93ms cold start) |

---

## THE ALGORITHM: Lifecycle-Aware Predictive Injection (LAPI)

### Core Formula

```
CRS(memory, query, phase) =
    w_sem(phase)  * semantic_similarity(query, memory)
  + w_rec(phase)  * temporal_decay(memory.age)
  + w_file(phase) * file_overlap(current_files, memory.files)
  + w_err(phase)  * error_match(current_errors, memory.errors)
  + w_proj        * project_affinity(current_project, memory.project)
```

### Phase-Specific Weight Vectors

| Signal | SessionStart | PromptSubmit | Stop | PreCompact |
|--------|-------------|-------------|------|------------|
| semantic | 0.30 | 0.35 | 0.25 | 0.20 |
| recency | 0.25 | 0.15 | 0.10 | 0.40 |
| file_overlap | 0.10 | 0.25 | 0.15 | 0.10 |
| error_match | 0.05 | 0.10 | 0.40 | 0.05 |
| project | 0.30 | 0.15 | 0.10 | 0.25 |

**SessionStart**: Heavy on project affinity + recency (show me what I worked on recently in THIS project)
**PromptSubmit**: Heavy on semantic + file overlap (help me with what I'm doing RIGHT NOW)
**Stop**: Heavy on error match (am I stuck? have I seen this before?)
**PreCompact**: Heavy on recency + project (preserve the current session's work)

### Why This Beats Everyone

1. **mem0**: Treats all retrieval equally. A memory about "Docker compose" has the same relevance score whether you're starting a session or debugging an error. LAPI weights differently.

2. **claude-mem**: Progressive disclosure is token-efficient but still reactive — Claude has to ASK. LAPI injects proactively at the right moment.

3. **Claude Code MEMORY.md**: Always loads everything. No selectivity at all.

---

## Temporal Scoring Upgrade (from Gemini research)

### Current: Simple exponential half-life
```rust
// score * ((1-0.3) + 0.3 * 2^(-age_days/90))
```

### Proposed: Spline-windowed decay (from MRAG 2024/Re³ 2025 research)
```rust
fn windowed_decay(age_days: f64) -> f64 {
    match age_days {
        d if d < 0.04  => 1.00,  // Last hour: full relevance
        d if d < 1.0   => 0.95,  // Today: near-full
        d if d < 7.0   => 0.85,  // This week: high
        d if d < 30.0  => 0.70,  // This month: moderate
        d if d < 90.0  => 0.55,  // This quarter: declining
        _ => 0.40,               // Older: baseline (never zero)
    }
    // Smooth interpolation between windows using hermite spline
}
```

Benefits: Recent memories get a much stronger boost without the exponential's rapid falloff. A 2-day-old memory scores 0.95 vs 0.98 under exponential — almost no difference. Under windowed, the "today" vs "this week" boundary creates meaningful differentiation.

---

## Story Coverage Fix: Three-Tier Synthesis (from Codex)

### Problem: Only 7/195 conversations have stories (3.6%)

### Solution: V3 data already contains stories

| Tier | Source | Coverage | Cost | Time |
|------|--------|----------|------|------|
| 1 | V3 `## Search Summary` → story | 129 convs (66%) | $0 | 250ms |
| 2 | Heuristic template → story | 36 convs (18%) | $0 | 36ms |
| 3 | Haiku (ambiguous only) | 5-10 convs | ~$0.04 | 75s |
| **Total** | | **94-97% of qualifying** | **~$0** | **<2min** |

### Implementation: `csr-engine backfill-stories`
1. Clean phantom enrichment_state rows
2. Synthesize stories from V3 Search Summary (Tier 1)
3. Template from heuristic data (Tier 2)
4. Optional `--haiku` flag for Tier 3

### Session-end integration
Try Tier 1 synthesis BEFORE spawning Haiku. Free, instant, often better quality.

---

## THE MOAT: Temporal Attention Decay (TAD) — from Codex

### The Insight
Standard decay treats all memories equally over time. But memories have different "attention curves" depending on whether they were USEFUL when retrieved.

A memory retrieved → acted upon → session succeeded should decay SLOWER.
A memory retrieved → acted upon → session FAILED should decay FASTER (but persist as anti-pattern).

### The Formula
```
effective_half_life = base_half_life * 2^(reinforcement_score)
reinforcement_score = Σ(outcome_weight * recency_of_retrieval)

outcome_weight: +1.0 (success), 0.0 (neutral), -1.0 (failure)
recency_of_retrieval: 2^(-days_since_retrieval / 30)
```

| Reinforcement | Effective Half-Life | Meaning |
|---------------|-------------------|---------|
| +2 (very useful) | 360 days | Persists nearly a year |
| 0 (standard) | 90 days | Default behavior |
| -2 (harmful) | 22.5 days | Fades fast, preserved as anti-pattern |

### Why This Is A Moat
1. **Data flywheel**: More usage → better decay curves → better retrieval → more usage
2. **Hard to replicate**: Needs hooks + outcome tracking + decay math (we have all three)
3. **Publishable**: "Temporal Attention Decay: Adaptive Memory Persistence Through Retrieval Outcome Tracking"
4. **Measurable**: A/B testable — TAD sessions should show higher success rates

### Schema Addition
```sql
CREATE TABLE retrieval_events (
    id TEXT PRIMARY KEY,
    memory_id TEXT NOT NULL,
    memory_type TEXT NOT NULL,       -- 'chunk' or 'reflection'
    retrieved_at TEXT NOT NULL,
    hook_phase TEXT NOT NULL,         -- which hook surfaced this
    session_outcome TEXT DEFAULT 'neutral',  -- updated by session_end
    session_id TEXT
);
```

Write path: log retrieval event when memory is injected (neutral).
Update path: session_end updates all events for that session with actual outcome.
Read path: JOIN retrieval_events when computing decay scores.

---

## Codex PCI Critique — 5 Issues Found

1. **Cross-project should be multiplicative, not additive** — high semantic + cross-project still scores well
2. **Phase weights need concrete numbers** — now defined (see Weight Profiles above)
3. **No diversity penalty** — top 5 results can all be same subtopic (need MMR)
4. **Error match is binary** — should be similarity score, not 0/1
5. **Recency double-counted** — decay.rs (90-day) and predictor.rs (30-day) both apply recency independently

### Fix: Unified Decay
```rust
DecayConfig::for_injection() → { weight: 0.5, half_life: 30 days }
DecayConfig::for_search()    → { weight: 0.3, half_life: 90 days }
```
Single function, TAD layered on top.

---

## Implementation Priority (from Codex)

| Order | Item | Effort | Value |
|-------|------|--------|-------|
| 1 | Template story generator | Small | Immediate (94% coverage) |
| 2 | Decay unification | Small | Fix recency double-counting bug |
| 3 | Weight profiles per hook phase | Medium | LAPI differentiation |
| 4 | TAD + retrieval event tracking | Large | The moat |

---

## Future Research Directions

### Graph-Based Memory Edges (from Gemini research on HippoRAG/TOBUGraph)
- When two conversations edit the same files → create edge
- When same error appears in two sessions → create edge
- Retrieval follows edges: "you fixed error X in conv Y which also touched file Z"
- Implementation: lightweight adjacency list in SQLite (not full Neo4j)

### Reinforcement from Access (from mem0's approach, improved)
- Track which injected memories the user actually USED (clicked through, referenced)
- Boost scores for memories that led to successful outcomes
- Decay memories that were injected but ignored
- Implementation: `access_count` and `last_accessed` columns in reflections table

### Conversation Flow Prediction
- Given the first 2-3 messages, predict what the user will need next
- Pre-load relevant memories before the user asks
- Implementation: embed the conversation opening, find similar past openings, inject what was needed in THOSE sessions
