# claude-mem Research Report

## Executive Summary
**claude-mem** is a persistent memory compression system for Claude Code with 50K+ GitHub stars. It's a mature competitor to Claude Self-Reflect that uses a different architecture (Node.js/Bun plugin system vs. Rust engine), different storage (SQLite + ChromaDB vs. Qdrant), and different retrieval strategy (progressive disclosure vs. efficient vector DB).

---

## 1. What It Is: Core Identity

### Repository
- **URL**: https://github.com/thedotmack/claude-mem
- **Stars**: 50K+ (fluctuating 45-50K)
- **Version**: 10.6.3 (as of April 2026)
- **Author**: Alex Newman (@thedotmack)
- **License**: AGPL-3.0
- **Language**: TypeScript/JavaScript (Node.js 18+, Bun runtime)

### What It Does
- **Primary Purpose**: Persistent memory plugin for Claude Code that automatically captures, compresses, and retrieves context across sessions
- **Key Claim**: Maintains Claude's continuity of knowledge about projects even after sessions end
- **Installation Method**: `/plugin marketplace add thedotmack/claude-mem` → `/plugin install claude-mem` in Claude Code
- **Also Available**: npm package (`npm install -g claude-mem`), but npm-only doesn't activate hooks; must use plugin system

### Key Statistics
- **10.6.3** release cycle (vs CSR v7.0 with Rust engine)
- **50K+ stars** (major competitor)
- **Version History**: v3 → v5 → v6+ (major evolution documented)
- **3,400+ observations** in production case study (23 days, 8 projects, two servers)

---

## 2. Memory Storage & Retrieval Architecture

### Two-Tier Hybrid Storage

#### Tier 1: SQLite Database (Primary)
- **Technology**: SQLite 3 with FTS5 (Full-Text Search 5) extension
- **Purpose**: Persistent storage of sessions, observations, summaries
- **Search Type**: Keyword/keyword-based via FTS5
- **Location**: `~/.claude-mem/claude-mem.db`
- **Problem**: Fragmentation over time → locking errors → manual `VACUUM` required

#### Tier 2: Chroma Vector Database (Optional)
- **Technology**: Chroma embeddings vector store
- **Purpose**: Semantic search & intelligent context retrieval
- **Search Type**: Cosine similarity (standard Chroma default)
- **When Activated**: On UserPromptSubmit hook via semantic context injection
- **Embedding Model**: Not explicitly specified in docs (likely OpenAI or Chroma's defaults)
- **Integration**: Recent feature (v6.5+) — "semantic context injection via Chroma on UserPromptSubmit"

### Progressive Disclosure Workflow (Token Efficiency)

Instead of injecting large memory blocks, claude-mem uses a **3-step retrieval pattern**:

```
Step 1: search()        → Returns compact index (IDs + titles) — 50-100 tokens
Step 2: timeline()      → Returns surrounding context (temporal anchoring)
Step 3: get_observations() → Fetches full text for specific IDs (on-demand only)
```

**Result**: ~10x token savings vs. standard RAG systems

### Lifecycle Hooks (Capture Points)
- **5 Main Hooks**:
  1. SessionStart — Injects context
  2. UserPromptSubmit — Semantic context injection (Chroma)
  3. PostToolUse — Captures tool outputs
  4. Stop — Iteration-level memory
  5. SessionEnd — Final summaries

- **Plus Hooks**: PreHook (dependency checker), Smart Install
- **Total Hook Scripts**: 6-7 compiled hooks

### Smart Compression Pipeline
- **Real-time Summarization**: Background worker uses Claude Agent SDK to compress tool outputs
- **Heuristic Layer**: Inline extraction before AI summarization
- **V3 Extraction**: Daemon-level processing (async)
- **Layer Supersession**: Each layer's reflection replaces previous in search index
- **Out-of-Band Processing**: Background workers on port 37777

---

## 3. System Architecture

### Components
1. **Lifecycle Hooks** (6 hook scripts) — Capture data at key points
2. **Worker Service** — HTTP API on port 37777 (Bun-managed)
3. **SQLite Database** — FTS5-indexed observation storage
4. **Chroma Vector DB** — Optional semantic search layer
5. **mem-search Skill** — Natural language query interface
6. **Web Viewer UI** — Real-time memory stream at http://localhost:37777

### Worker Service API (Port 37777)
- **10 Search Endpoints**: Various query patterns
- **Web UI**: Real-time observation viewer
- **Skill Integration**: mem-search MCP skill
- **Managed By**: Bun runtime (compiled supervisor)

### Technology Stack
- **Runtime**: Node.js 18+ or Bun 1.0+
- **Plugin System**: Claude Code plugin manifest
- **Serialization**: Likely JSON
- **Process Management**: Supervisor daemon (similar to CSR but in Node/Bun)

---

## 4. Retrieval Algorithm & Search Strategy

### Search Modes
1. **Keyword Search** (SQLite FTS5)
   - Exact/fuzzy keyword matching
   - Full-text search across observations
   - Fast but semantic-blind

2. **Semantic Search** (Chroma)
   - Cosine similarity on embeddings
   - Enabled on UserPromptSubmit
   - Slower but contextually aware

3. **Progressive Disclosure**
   - 3-step retrieval (index → timeline → full)
   - Token budgeted
   - Prevents context window exhaustion

### Retrieval Scoring (Inferred)
- **Hybrid Approach**: Combines FTS5 rank with Chroma cosine similarity
- **Tier Routing**: Recent feature (#1569) — "tier routing by queue complexity + observation feedback table"
- **Thompson Sampling**: RFC (#1571) for observation quality optimization
- **NOT Simple Cosine**: Uses feedback table + Thompson Sampling for intelligent ranking

---

## 5. Limitations & Known Issues (From GitHub)

### Critical Issues

#### 1. **Database Locking & Fragmentation**
- **Problem**: SQLite locking errors in multi-process architecture
- **Root Cause**: High-frequency inserts/updates fragment database
- **Workaround**: Manual `VACUUM`, kill dangling processes via `lsof`
- **Impact**: Productivity lost to maintenance
- **Issue #**: Recurring across v6.x releases

#### 2. **Worker Daemon Hangs (#1587)**
- **Problem**: Worker daemon hangs indefinitely on startup
- **Root Cause**: `isPortInUse()` in HealthMonitor.ts calling `fetch()` with NO TIMEOUT
- **Affected**: Bun 1.3.11 on Linux/WSL2 (hangs forever instead of throwing ECONNREFUSED)
- **Date**: April 3, 2026 (brand new)
- **Impact**: Plugin broken on Linux

#### 3. **Stale Context Preview (#1565)**
- **Problem**: SessionStart injects old observations (Mar 30) instead of recent (Apr 2)
- **Root Cause**: Timeline sorted ascending (oldest first) → preview truncated
- **Impact**: User sees outdated context, defeats compression benefits
- **Date**: April 1, 2026

#### 4. **Memory Rot & Irrelevant Retrieval**
- **Problem**: System retrieves outdated/irrelevant memories (3-week-old debugging session instead of current codebase state)
- **Root Cause**: Embedding quality drops when mixing tool logs + code + natural language (different semantic densities)
- **Similar to CSR Bug 7**: Tool-heavy chunks causing severe retrieval issues
- **Community Discussion**: Users propose "hippocampal consolidation" or confidence decay algorithms

#### 5. **Context Window Exhaustion**
- **Problem**: Despite compression, raw tool outputs (10k+ tokens) can overflow context
- **When**: Aggressive tasks (heavy grep, massive refactors) with multiple dense tool uses in succession
- **Progressive Disclosure Helps**: But doesn't fully solve raw ingestion problem

#### 6. **Decay Algorithm Issues**
- **Problem**: Append-only persistence leads to crowded vector space
- **Missing Feature**: No natural forgetting mechanism (unlike CSR's decay formula)
- **Community Request**: Daily decay penalty (-0.05/day) + reinforcement on access
- **Status**: Community discussions (#283, #841 in related ecosystems), not implemented

#### 7. **Missing Hook Output Fallback (#1560)**
- **Problem**: SessionStart hook fails when Claude Code doesn't pipe stdin
- **TTY Check**: Missing in some contexts
- **Status**: Under investigation

#### 8. **Bare Path Handling (#1554)**
- **Problem**: `files_modified`/`files_read` columns don't handle bare filenames
- **Impact**: Missing context for certain file operations
- **Date**: April 1, 2026

#### 9. **MCP Tool Schema Missing (#1555)**
- **Problem**: `search` and `timeline` MCP tools lack proper `inputSchema` properties
- **Impact**: Tool discovery/validation failures
- **Date**: April 1, 2026

#### 10. **Chrome Vector DB Sync Issues (#1566)**
- **Problem**: 3 upstream bugs in summarize, ChromaSync, HealthMonitor
- **Status**: "Need to resolve"
- **Date**: April 2, 2026

### Design Limitations

#### SQLite-Centric Architecture
- **Advantage**: Simplicity, ACID guarantees
- **Disadvantage**: Single database file → fragmentation → locking → performance cliff
- **Comparison to CSR**: Qdrant is purpose-built for vector search, avoids these issues

#### Chroma Integration (Optional)
- **Advantage**: Semantic search capability
- **Disadvantage**: Optional (many users don't enable), not integrated by default
- **Problem**: Dual storage = dual sync complexity (see #1566 ChromaSync bugs)

#### Token Budgeting (Progressive Disclosure)
- **Advantage**: 10x token savings claimed
- **Disadvantage**: Multi-step retrieval adds latency
- **Comparison to CSR**: Direct injection is faster, but CSR has decay to prevent bloat

#### Append-Only Persistence
- **Advantage**: No data loss
- **Disadvantage**: Vector space crowding, memory rot over time
- **CSR Advantage**: Decay formula + supersession patterns prevent this

---

## 6. Comparison: claude-mem vs Claude Self-Reflect

### Architecture
| Aspect | claude-mem | Claude Self-Reflect |
|--------|-----------|-------------------|
| Runtime | Node.js + Bun | Rust |
| Storage | SQLite + Chroma | Qdrant vector DB |
| Search | FTS5 + cosine similarity | HNSW vector search + FTS |
| Plugin System | Claude Code `/plugin` | Hooks + MCP server |
| Process Model | Multi-process (daemon) | Async/await in Rust |
| Persistence | File-based SQLite | Vector index dump + DB |

### Strengths & Weaknesses

#### claude-mem Strengths
1. **Mature ecosystem** — 50K stars, large community
2. **Progressive disclosure** — Token-efficient retrieval
3. **Observation feedback table** — Thompson Sampling for quality
4. **Web UI** — Real-time memory visualization
5. **Cooperative cycling** — Multi-project context sharing

#### claude-mem Weaknesses
1. **SQLite locking** — Fragmentation under load
2. **Worker daemon hangs** — Broken on Linux (as of Apr 3)
3. **Memory rot** — No decay, vector space bloating
4. **Stale previews** — Users see outdated context
5. **Optional semantics** — Chroma integration is opt-in, not default
6. **Embedding quality** — Tool + code + text mixing causes noise

#### CSR Strengths (vs claude-mem)
1. **Qdrant-native** — Purpose-built for vector search, no fragmentation
2. **Decay algorithm** — Prevents memory rot (formula-based)
3. **Direct injection** — No multi-step latency (cache-first design)
4. **Tool context extraction** — 2.8x more chunks from tool metadata
5. **HNSW persistence** — Efficient index caching (153x speedup with fs2 locking)
6. **Layer supersession** — Replaces old versions cleanly

#### CSR Weaknesses (vs claude-mem)
1. **Newer** — Less battle-tested (Rust spike started Jan 2026)
2. **Smaller community** — No 50K stars yet
3. **Token visibility** — Progressive disclosure shows exact token costs
4. **MCP resource API** — New (May help competitive positioning)
5. **OpenClaw integration** — claude-mem has curl installer, CSR doesn't

### User Migration Patterns
- **No evidence** of claude-mem users switching to CSR (too recent)
- **Likely switch drivers**: 
  - SQLite stability issues
  - Memory rot complaints
  - Linux daemon hangs
  - Desire for Rust/native performance

---

## 7. User Complaints (Detailed)

### From GitHub Issues & Discussions

#### Performance & Stability
1. **Slow search as DB grows** (recurring)
   - "Semantic searches and memory injections slow down significantly"
   - Root: SQLite fragmentation

2. **Worker crashes on startup** (#1587, Apr 3)
   - "Worker daemon hangs on startup: fetch() has no timeout"
   - "hangs forever on Bun 1.3.11/Linux"
   - "port 37777 never binds"

3. **Database locking errors**
   - Users running `lsof` to kill dangling processes
   - Manual `VACUUM` required periodically

#### Memory Quality
4. **Memory rot** (3+ discussions)
   - "System retrieves outdated debugging sessions instead of current codebase"
   - "Memory doesn't reflect reality after 1-2 weeks"

5. **Irrelevant retrieval**
   - Tool logs + code embeddings = semantic noise
   - Keyword search pulls unrelated sessions

6. **Stale context preview** (#1565, Apr 1)
   - "User sees observations from 3 days ago instead of today's session"
   - "SessionStart hook preview truncated"

#### Usability Issues
7. **No 'Beta Channel'** (#1562, Apr 1)
   - Users want Endless Mode but can't access it
   - Feature exists but not discoverable

8. **Missing TTY fallback** (#1560, Apr 1)
   - "Hook fails when Claude Code doesn't pipe stdin"
   - "SessionStart error: stdin not available"

9. **Incomplete file path handling** (#1554, Apr 1)
   - "Bare filenames not stored in files_modified"
   - "Missing context for file operations"

#### Integration Issues
10. **MCP tool schema missing** (#1555, Apr 1)
    - "`search` and `timeline` tools lack inputSchema"
    - "Tool discovery failures"

11. **ChromaSync bugs** (#1566, Apr 2)
    - "3 upstream bugs in summarize, ChromaSync, HealthMonitor"
    - "Dual sync complexity between SQLite and Chroma"

### Not Documented But Inferred
- **No built-in decay** → users manually deleting old sessions
- **Token visibility** → users manually calculating costs
- **Vector space bloat** → performance cliff after 3,400+ observations
- **Single-machine limitation** → #1570 RFC for multi-machine sync (NEW)

---

## 8. Unique Features (vs CSR)

### claude-mem Exclusive
1. **Endless Mode** (Beta) — Biomimetic architecture, O(N) scaling instead of O(N^2)
2. **Thompson Sampling** (RFC #1571) — Probabilistic observation quality optimization
3. **Cooperative Cycling** (#1577) — Share context across multiple worktrees of same repo
4. **Storyline Content Ingestion** (#1577) — Special media format ingestion
5. **Web Viewer UI** — Real-time observation dashboard (http://localhost:37777)
6. **Tier Routing** (#1569) — Queue complexity-based routing
7. **Multi-machine Sync** (#1570) — claude-mem-sync for distributed observation sharing
8. **OpenClaw Gateway** — Pre-built cloud integration (curl install)

### CSR Exclusive
1. **Rust engine** — Native performance, no Node.js overhead
2. **Decay algorithm** — Built-in formula for natural forgetting
3. **Tool context extraction** — 2.8x more chunks via metadata
4. **HNSW persistence** — 153x faster cache loading
5. **Quality CLI** — `csr-engine quality <path>` (6 languages)
6. **Eval framework** — 20-test evaluation suite (p95: 0.82ms)
7. **MCP Resources** — `status://system-health` resource
8. **AST analysis** — Function/type/import extraction from conversations
9. **Layer supersession** — Clean multi-layer replacement
10. **Hooks installer** — `csr-engine hook install` (verified reproducible)

---

## 9. Retrieval Algorithm Deep Dive

### claude-mem Search Flow
```
Query: "How do I fix Docker memory?"
↓
1. FTS5 Keyword Search (SQLite)
   → Returns: {ID: 42, title: "Docker container OOM", date: "Mar 31"}
   → Cost: ~50 tokens
↓
2. Timeline Context (if relevant)
   → Returns: Entries from Mar 30-31 window
   → Cost: ~100 tokens
↓
3. Full Observation (on-demand)
   → Returns: Complete tool output + notes
   → Cost: ~500 tokens (only if explicitly requested)
```

### Scoring (Estimated from RFCs)
- **Base**: SQLite FTS5 rank + Chroma cosine similarity
- **Reinforcement**: Access frequency boost
- **Decay**: Community requested (-0.05/day) but NOT implemented
- **Thompson Sampling**: Proposed (#1571) but status unclear
- **Observation Feedback**: Tier routing uses this (#1569)

### Comparison to CSR
- **CSR**: Direct HNSW vector search → cached index (93ms cached, 14s cold)
- **claude-mem**: 3-step retrieval → token budgeted, slower but cheaper
- **Winner**: Depends on use case
  - CSR faster for cache-hit scenarios
  - claude-mem cheaper for token budgets

---

## 10. Integration & Extensibility

### Plugin Ecosystem
- **MCP Tools**: 10+ search endpoints via MCP server
- **Skills**: mem-search skill for natural language queries
- **Desktop**: Claude Desktop skill available
- **Web**: Localhost UI at port 37777

### Multi-Machine
- **New Feature** (#1570): claude-mem-sync for observation synchronization
- **Use Case**: Share memories across machines (not in CSR yet)

### API Surface
- **SDK Export**: `./sdk` export for programmatic use
- **Modes**: Documented in `plugin/modes/*`
- **CLI**: Various hooks as subcommands

---

## 11. Competitive Positioning Summary

### claude-mem Positioning
- **Brand**: "The mature, battle-tested memory system for Claude"
- **Audience**: Users comfortable with Node.js, prefer web UI, want feature richness
- **Claim**: 50K stars, proven in 8-project production, 3,400+ observations
- **Weakness**: SQLite stability, memory rot, Linux daemon bugs

### CSR Positioning
- **Brand**: "The high-performance, zero-decay memory engine"
- **Audience**: Developers wanting Rust reliability, low-latency search, native performance
- **Claim**: Faster (0.82ms p95), decay prevents memory rot, Qdrant-native
- **Weakness**: Newer, smaller community, less feature-rich

### Market Opportunity
1. **Migration from claude-mem**: SQLite stability issues + memory rot complaints
2. **First-time users**: May choose CSR if performance/reliability emphasized
3. **Enterprise**: CSR's Rust + Qdrant more appealing than Node.js + SQLite
4. **Feature parity**: CSR needs Web UI to match claude-mem visibility

---

## 12. Research Sources

- **Primary**: https://github.com/thedotmack/claude-mem
- **Package**: v10.6.3 (as of April 2026)
- **Issues**: #1587 (worker hang), #1565 (stale preview), #1577 (cooperative cycling), #1570 (multi-machine sync)
- **RFCs**: #1571 (Thompson Sampling), #1569 (tier routing)
- **Case Study**: #1573 (23 days production, 3,400+ observations)
- **Recent Commits**: Auto-sync docs, Codex plugin manifest, Storyline ingestion

---

## Conclusions

### What We're Up Against
1. **50K stars** — Large, visible competitor
2. **Mature architecture** — Proven despite SQLite issues
3. **Rich feature set** — Web UI, multi-project, Endless Mode, Thompson Sampling
4. **Production track record** — 8 projects, 23 days, 3,400+ observations documented

### Our Competitive Advantages
1. **Rust engine** — Native performance, no Node.js overhead
2. **Decay algorithm** — Built-in forgetting prevents memory rot
3. **Qdrant integration** — Purpose-built vector DB (no SQLite fragmentation)
4. **HNSW caching** — 153x faster cached searches
5. **Tool context extraction** — 2.8x more searchable chunks
6. **Zero locking issues** — fs2 advisory locks vs SQLite fragmentation

### Recommended Positioning
- **Lead with**: "Qdrant-native, zero-decay, Rust-powered memory engine"
- **Emphasize**: Performance (0.82ms p95) + stability (no SQLite locking)
- **Challenge**: Memory rot problems in SQLite-based systems
- **Expand**: Add Web UI parity (localhost dashboard like claude-mem)
- **Highlight**: Tool context extraction (unique to CSR)

### Next Steps for CSR
1. Build Web UI at localhost port (match claude-mem UX)
2. Document decay formula vs append-only systems
3. Case study: CSR vs claude-mem performance benchmark
4. OpenClaw-style integration guide
5. RFC for multi-machine sync (like #1570)
