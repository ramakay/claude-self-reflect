# Claude-Mem Quick Reference Card

## At a Glance

| Aspect | claude-mem | CSR (Claude Self-Reflect) |
|--------|-----------|--------------------------|
| **Stars** | 50K+ | Emerging |
| **Runtime** | Node.js + Bun | Rust |
| **Storage** | SQLite + Chroma | Qdrant |
| **Search** | FTS5 + cosine | HNSW + FTS |
| **Decay** | None (app-only) | Built-in formula |
| **Search Speed** | Multi-step (token budgeted) | Direct (93ms cached) |
| **Linux Stability** | Broken (#1587) | Native |

## GitHub Repository
**https://github.com/thedotmack/claude-mem**

## What It Is
- Persistent memory plugin for Claude Code
- Captures, compresses, and retrieves context across sessions
- Install via `/plugin install claude-mem` (not npm install)

## Storage Architecture

### Primary: SQLite + FTS5
- Fragmentation issue under heavy load
- Manual `VACUUM` required periodically
- Locking errors in multi-process architecture

### Optional: Chroma Vector DB
- Cosine similarity search
- Dual storage = sync complexity
- Recently added (v6.5+)

## Retrieval: Progressive Disclosure (3-Step)
```
Step 1: search()  → Index (50-100 tokens)
Step 2: timeline() → Context (temporal anchoring)
Step 3: get_observations() → Full text (on-demand)
```
Claimed 10x token savings vs standard RAG.

## Top 5 Known Issues

### Critical (Production-Affecting)
1. **#1587** (Apr 3, 2026): Worker daemon hangs on Linux
   - fetch() has no timeout
   - Port 37777 never binds
   - STATUS: Unresolved

2. **#1565** (Apr 1, 2026): Stale context preview
   - Timeline sorted ascending
   - Users see 3-day-old observations
   - STATUS: Unresolved

3. **#1566** (Apr 2, 2026): ChromaSync bugs
   - 3 upstream bugs: summarize, ChromaSync, HealthMonitor
   - Dual storage sync complexity
   - STATUS: Under investigation

### User Pain Points
4. **Memory Rot**: Append-only system retrieves outdated memories
   - No decay mechanism
   - Vector space bloating after 3,400+ observations
   - Community wants hippocampal consolidation

5. **Embedding Quality**: Tool logs + code + text mixing = semantic noise
   - Similar to CSR's documented Bug 7
   - Causes irrelevant retrieval

## Unique Features

### claude-mem Has
- Endless Mode (O(N) scaling, beta)
- Thompson Sampling RFC
- Cooperative Cycling (multi-worktree)
- Web Viewer UI (localhost:37777)
- 50K star community

### CSR Has
- Rust engine (native performance)
- Decay algorithm (built-in forgetting)
- Tool context extraction (2.8x more chunks)
- HNSW caching (153x faster)
- Quality CLI (6 languages)
- Eval framework (20 tests)

## Competitive Advantages (CSR vs claude-mem)

### Performance
- CSR: Direct HNSW search, 93ms cached
- claude-mem: 3-step retrieval, slower but token-cheaper

### Stability
- CSR: Qdrant (no SQLite locking)
- claude-mem: SQLite fragmentation issues

### Memory Quality
- CSR: Decay formula prevents rot
- claude-mem: Append-only, vector space bloating

### Searchability
- CSR: Tool context extraction (2.8x more chunks)
- claude-mem: Raw tool logs + text (embedding quality suffers)

## Market Opportunity

### Migration Drivers
1. SQLite stability (fragmentation, locking)
2. Memory rot (no decay)
3. Linux daemon hangs (#1587)
4. Embedding quality problems

### Positioning Strategy
- Lead: "Qdrant-native, zero-decay, Rust-powered"
- Emphasize: Performance (0.82ms p95) + stability
- Challenge: Memory rot in append-only systems
- Expand: Add Web UI parity

## Key GitHub Issues to Monitor

| Issue | Title | Status | Date |
|-------|-------|--------|------|
| #1587 | Worker daemon hangs on startup | Unresolved | Apr 3 |
| #1565 | Session context preview stale | Unresolved | Apr 1 |
| #1566 | ChromaSync bugs | Investigation | Apr 2 |
| #1560 | Hook fails without stdin | Investigation | Apr 1 |
| #1555 | MCP tool schema missing | Unresolved | Apr 1 |
| #1571 | Thompson Sampling RFC | Proposed | Apr 2 |
| #1570 | Multi-machine sync RFC | Proposed | Apr 2 |
| #1577 | Cooperative cycling | In Progress | Apr 2 |

## Full Analysis Location
`/Users/ramakrishnanannaswamy/projects/claude-self-reflect/docs/analysis/claude-mem-competitive-analysis.md`

Contains:
- 474 lines of detailed research
- Architecture deep dive
- All 10 major issues documented
- Feature-by-feature comparison
- Positioning recommendations
- Next steps for CSR

---

**Research Completed:** April 3, 2026  
**Data Freshness:** Current as of latest GitHub activity (April 3, 2026)  
**Sources:** GitHub API, repository files, issues, RFCs
