# Documentation Redesign Plan -- Claude Self-Reflect

> Date: 2026-04-25
> Author: Documentation Audit Agent
> Scope: Full inventory, gap analysis, GitHub Pages site design, migration plan

---

## Part 1: Current Documentation Inventory

### Summary Statistics

| Category | Count | Quality Range | Notes |
|----------|-------|---------------|-------|
| Root-level docs | 5 | 2-4 | README strong, rest outdated |
| Release notes | 57 | 2-3 | Massive sprawl, most historical |
| Testing reports | 35 | 1-2 | Internal artifacts, not user-facing |
| Architecture/design | 12 | 2-4 | Mixed Python/Rust, some strong |
| Guides (install, setup, etc.) | 14 | 2-4 | Mostly reference old Python stack |
| Development docs | 18 | 2-3 | Internal dev logs, version-specific |
| Feature docs | 8 | 3-4 | Good concepts, outdated details |
| Operations | 16 | 1-2 | Deployment artifacts |
| Planning docs | 12 | 2-3 | Internal, not user-facing |
| Troubleshooting | 8 | 2-3 | Some useful, most reference Docker/Python |
| Research/analysis | 8 | 3-4 | Good conceptual content |
| Announcements | 7 | 2 | Historical, version-specific |
| Internal (.internal/) | 3 | 3-4 | Strategic, correctly private |
| Rust engine plans (.plans/) | 7 | 4 | Active, well-structured |
| csr-engine/ docs | 4 | 3-4 | Platform research, issues log |
| **TOTAL** | ~246 | -- | -- |

### Detailed File Inventory

#### Root-Level Documents

| File | Purpose | Quality (1-5) | Status |
|------|---------|---------------|--------|
| `README.md` | Project overview, quick start, tool reference | **4** | Current with Rust engine. Well-structured, good before/after, performance table. Missing: contributor quickstart, deeper architecture link. |
| `CONTRIBUTING.md` | Contributor guide | **2** | OUTDATED. References Docker, Node.js, Python stack. Mentions `npm run dev`, `docker compose up`. No mention of Rust, `csr-engine/`, or `cargo`. |
| `SECURITY.md` | Security policy | **2** | OUTDATED. Supported versions list says 2.3.x. References Docker images, Trivy, npm audit. |
| `RELEASE_NOTES.md` | v2.5.16 release notes | **2** | STALE. This is for v2.5.16 only. Not a changelog covering recent versions. |
| `CLAUDE.md` | Agent instructions | **5** | Current, extremely detailed, well-maintained. Not a user doc -- internal agent guide. |

#### Core User Guides (docs/)

| File | Purpose | Quality | Status |
|------|---------|---------|--------|
| `docs/installation-guide.md` | Full installation walkthrough | **2** | OUTDATED. References Docker, Python venv, npm, Voyage AI as primary. No Rust binary. |
| `docs/api-reference.md` | MCP tool reference | **3** | Partially outdated. Documents `reflect_on_past` and `store_reflection` well but only covers 2 of 12 tools. Uses Python API signatures. |
| `docs/troubleshooting.md` | Common issues and fixes | **2** | OUTDATED. All solutions reference Docker, Python, npm. No Rust troubleshooting. |
| `docs/advanced-usage.md` | Power user features | **3** | Concepts are good (search strategies, cross-project). Details reference old stack. |
| `docs/MCP_SETUP_GUIDE.md` | MCP server configuration | **2** | OUTDATED. Docker MCP and Python local MCP instructions. No `csr-engine` binary. |
| `docs/UNINSTALL.md` | Removal instructions | **2** | OUTDATED. References docker-compose, npm uninstall, Python artifacts. |
| `docs/windows-setup.md` | Windows WSL guide | **2** | OUTDATED. Docker Desktop + WSL + npm approach. |
| `docs/performance-guide.md` | Performance tips | **2** | OUTDATED. References Docker memory, reflection-specialist agent, 200-350ms. |
| `docs/memory-decay.md` | Decay algorithm explanation | **4** | Good conceptual content. Math is correct. Implementation details reference Qdrant Formula API (Python), but the Rust engine uses the same algorithm. Needs code update. |
| `docs/embedding-migration.md` | Switching local/cloud embeddings | **2** | OUTDATED. References Qdrant collections, Voyage AI, Python scripts. |
| `docs/project-scoped-search.md` | How project detection works | **3** | Concepts correct. Detection logic is the same. Python code examples need update. |
| `docs/UNIFIED_STATE_MIGRATION_GUIDE.md` | v5.0 state migration | **1** | OBSOLETE. Rust engine uses SQLite, not JSON state files. |
| `docs/session-startup-hook.md` | Auto-indexing hook | **2** | OUTDATED. References Python hook scripts, old settings format. |
| `docs/precompact-hook-setup.md` | PreCompact hook guide | **2** | OUTDATED. References Python scripts and old hooks directory. |

#### Architecture & Conceptual Documents

| File | Purpose | Quality | Status |
|------|---------|---------|--------|
| `docs/architecture-details.md` | System architecture | **2** | OUTDATED. ASCII diagrams show Python FastMCP + Qdrant stack. |
| `docs/components.md` | Component deep dive | **2** | OUTDATED. References Qdrant, Python, FastMCP. |
| `docs/streaming-importer-architecture.md` | Streaming import design | **2** | OBSOLETE. Python async architecture, no longer used. |
| `docs/theoretical-foundation.md` | SPAR framework alignment | **4** | Good conceptual content, mostly framework-agnostic. Minor references to Python components. |
| `docs/motivation-and-history.md` | Project history | **4** | Good narrative. Needs v8.0 Rust chapter added. |
| `docs/QUALITY_AUTOMATION.md` | AST-GREP quality system | **3** | Good overview. References Python scripts but Rust engine has `csr-engine quality`. |

#### Feature Documentation

| File | Purpose | Quality | Status |
|------|---------|---------|--------|
| `docs/features/hot-warm-cold.md` | Import prioritization | **3** | Good concept. Implementation now in Rust `watcher.rs`. |
| `docs/features/code-quality-monitoring.md` | Quality feedback | **3** | Conceptually relevant. |
| `docs/features/statusline-integration.md` | Statusline setup | **3** | `csr-engine status --compact` replaces Python statusline. |
| `docs/features/REAL_TIME_QUALITY_FEEDBACK.md` | Quality feedback | **3** | Overlap with quality monitoring. |
| `docs/docker-watcher-guide.md` | Docker watcher service | **1** | OBSOLETE. Entire Docker watcher replaced by `csr-engine --watch`. |

#### Hooks Documentation

| File | Purpose | Quality | Status |
|------|---------|---------|--------|
| `docs/development/HOOKS_DOCUMENTATION.md` | Hooks overview | **2** | Documents OLD Python/.claude/hooks/ system, not the Rust `csr-engine hook` system. |

#### Development & Internal Docs

| File | Purpose | Quality | Status |
|------|---------|---------|--------|
| `docs/development/MCP_REFERENCE.md` | MCP tool naming conventions | **3** | Tool naming rules still valid. Tool list incomplete (6 of 12). References Python env vars. |
| `docs/development/AGENTS_DOCUMENTATION.md` | Agent registry | **3** | Good reference for agent capabilities. |
| `docs/development/AST_GREP_REGISTRY_DOCUMENTATION.md` | AST-GREP patterns | **3** | References Python scripts. Rust engine has built-in AST analysis. |
| `docs/development/STATE_CONSOLIDATION_SUMMARY.md` | State file consolidation | **1** | OBSOLETE. About Python state files. |
| `docs/contributing-and-release.md` | Release process | **2** | References old npm-only release. No Rust binary CI/CD. |
| `docs/security.md` | Security models | **3** | Standalone vs Shared model still conceptually valid. Details reference Docker/Qdrant. |

#### Release Notes (57 files)

All release notes from v2.0 through v6.0.4 exist in:
- `docs/RELEASE_NOTES_v*.md` (30+ files)
- `docs/operations/RELEASE_NOTES_*.md` (12+ files)
- `docs/releases/v*.md` (2 files)
- Root `RELEASE_NOTES.md` (1 file, v2.5.16 only)

**Quality**: 2-3. These are historical artifacts. They document Python/Docker/Qdrant versions.
**Recommendation**: Archive into a single `CHANGELOG.md`. Only v8.0+ needs active documentation.

#### Testing Reports (35 files in docs/testing/)

All are historical test reports from the Python era (September 2025 through early 2026).
**Quality**: 1-2 for current relevance. Internal artifacts.
**Recommendation**: Archive or delete entirely. Rust engine has its own test suite (`cargo test`, 273+ tests).

#### Rust Engine Plans (.plans/)

| File | Purpose | Quality | Status |
|------|---------|---------|--------|
| `.plans/00-rust-rewrite-master-plan.md` | Master plan for Rust rewrite | **4** | Comprehensive strategy doc. |
| `.plans/01-phase0-rust-spike.md` | Phase 0 spike plan | **4** | Good reference. |
| `.plans/02-phase0-go-nogo-results.md` | Benchmark results | **4** | GO decision documented. |
| `.plans/03-phase-tracker.md` | Phase progress tracker | **4** | Phases 0-4 tracked. Current. |
| `.plans/05-algorithm-research.md` | Algorithm research | **3** | Research notes. |
| `.plans/SESSION-HANDOFF.md` | Session continuity | **3** | Internal. |
| `.plans/phase-2.5-issues.md` | Issue tracking | **3** | Internal. |

#### csr-engine/ Documents

| File | Purpose | Quality | Status |
|------|---------|---------|--------|
| `csr-engine/ISSUES.md` | Bug log from dogfooding | **4** | Excellent real-world bug documentation. |
| `csr-engine/data/SKILL_V2.md` | AI narrative prompt template | **4** | Active, used by daemon. |
| `csr-engine/docs/platform-research/*.md` | ORT platform support research | **4** | Current, well-researched. |

#### Research Documents

| File | Purpose | Quality | Status |
|------|---------|---------|--------|
| `docs/research/CSR_NOVEL_ALGORITHMS.md` | OBRL, CFP, HMC algorithms | **4** | Forward-looking research. |
| `docs/research/IMPLEMENTATION_SUMMARY.md` | Algorithm implementation summary | **4** | Good technical depth. |
| `docs/research/RUST_IMPLEMENTATION_PATTERNS.md` | Rust patterns reference | **3** | Internal reference. |

---

## Part 2: Information Gaps

### Critical Gaps (Must Have for v8.0 Launch)

1. **Rust Engine Architecture Guide** -- No document explains the Rust binary architecture (SQLite + HNSW + FastEmbed + MCP server in one process). The old `architecture-details.md` describes the defunct Python/Docker/Qdrant stack.

2. **Complete MCP Tool Reference** -- `api-reference.md` covers 2 of 12 tools. The README table lists all 12 but with one-line descriptions. No parameter docs, examples, or return values for 10 tools.

3. **Hooks System Guide (Rust)** -- The 6 Rust hooks (`csr-engine hook session-start|session-end|precompact|stop|post-tool-use|prompt-submit`) have zero user-facing documentation. `HOOKS_DOCUMENTATION.md` describes the old Python hooks.

4. **Updated Installation Guide** -- Current guide describes Docker + npm + Python. The actual install is `curl | sh` + `csr-engine setup`.

5. **CLI Reference** -- README has a brief table. No detailed CLI docs with all flags, options, and examples for `setup`, `status`, `daemon`, `hook install`, `eval`, `quality`, `--import`, `--enrich`, `--watch`.

6. **Enrichment Pipeline Guide** -- The 3-layer enrichment (heuristic -> V3 extraction -> AI narrative) is mentioned in README but has no dedicated guide explaining what each layer does, how to configure, or when to use AI narratives.

7. **Contributing Guide (Rust)** -- `CONTRIBUTING.md` describes the old stack. New contributors need: Rust toolchain setup, `cargo test`, project structure walkthrough, PR process.

### Important Gaps (High Value)

8. **Configuration Reference** -- No single document lists all configuration options (environment variables, CLI flags, settings.json hooks config).

9. **Migration Guide (v7 to v8)** -- README has a 4-step upgrade section. Needs fuller treatment covering data continuity, what gets deleted, rollback path.

10. **Search Quality Guide** -- How to get the best search results. Covers: query writing tips, when to use which tool, understanding scores, project scoping, cross-project search.

11. **Ralph Loop Integration Guide** -- CLAUDE.md has extensive Ralph loop documentation but no user-facing guide. The README mentions it briefly.

12. **Eval Framework Guide** -- `csr-engine eval` and `csr-engine eval --full` are mentioned but no guide explains the evaluation tasks, how to interpret results, or how to add custom evaluations.

### Nice-to-Have Gaps

13. **Comparison Page** -- How CSR compares to claude-mem, Cursor memory, official Anthropic memory. The `.internal/` docs have competitive analysis but nothing public.

14. **FAQ** -- Common questions aggregated from issues, troubleshooting, and community feedback.

15. **Video / GIF Demos** -- The `video/` directory has a parallel-agents-hero project, but no actual demo content linked from docs.

16. **Changelog** -- 57 release notes but no unified changelog.

---

## Part 3: Proposed Documentation Site Structure

### Information Architecture

```
Claude Self-Reflect Docs
|
+-- Getting Started
|   +-- What is CSR?                     (value prop, before/after, how it works)
|   +-- Installation                     (curl|sh, csr-engine setup, verify)
|   +-- Quick Start                      (first search, first store, first hook)
|   +-- Upgrading from v7.x             (migration guide)
|
+-- Guides
|   +-- Search Strategies               (query writing, tools by use case, scores)
|   +-- Hooks System                     (6 hooks, installation, customization)
|   +-- AI Narratives                    (3-layer enrichment, API key, daemon)
|   +-- Ralph Loop Memory               (setup, cross-session memory, anti-patterns)
|   +-- File Watcher                     (auto-import, hot/warm/cold, configuration)
|   +-- Code Quality Analysis            (AST analysis, quality CLI)
|   +-- Evaluation Framework             (eval, eval --full, custom tests)
|   +-- Windows Setup                    (WSL instructions, path handling)
|
+-- Reference
|   +-- MCP Tools                        (all 12 tools, parameters, examples, returns)
|   +-- CLI Reference                    (every subcommand with flags)
|   +-- Configuration                    (env vars, settings.json, .env)
|   +-- Hooks API                        (stdin JSON schema, stdout injection format)
|
+-- Architecture
|   +-- System Overview                  (single binary, components, data flow)
|   +-- Storage Layer                    (SQLite schema, HNSW index, caching)
|   +-- Embedding Engine                 (FastEmbed, all-MiniLM-L6-v2, batch processing)
|   +-- Search Engine                    (HNSW, decay, cross-project, filtering)
|   +-- Enrichment Pipeline              (3 layers, daemon, Batch API)
|   +-- Memory Decay                     (algorithm, formula, configuration)
|   +-- Design Decisions                 (why Rust, why no Docker, why HNSW over Qdrant)
|
+-- Contributing
|   +-- Development Setup                (Rust toolchain, cargo test, project structure)
|   +-- Code Style                       (clippy, fmt, pre-commit hooks)
|   +-- Testing Guide                    (unit, integration, hooks, benchmarks)
|   +-- Release Process                  (CI/CD, binary builds, npm publish)
|   +-- Security Policy                  (reporting, supported versions)
|
+-- About
|   +-- Motivation & History             (why we built it, what failed, evolution)
|   +-- Research                         (OBRL, CFP, HMC algorithms)
|   +-- Theoretical Foundation           (SPAR framework, biological inspiration)
|   +-- Comparison                       (vs claude-mem, vs Cursor, vs official memory)
|   +-- FAQ
|
+-- Changelog                            (consolidated from 57 release notes)
```

### Navigation Design

**Top navigation**: Getting Started | Guides | Reference | Architecture | Contributing

**Sidebar** (within each section): All pages in that section, with the current page highlighted.

**Search**: Full-text search across all documentation (built into framework).

**Footer**: GitHub link, npm link, MIT license, "Built by ramakay"

---

## Part 4: Framework Recommendation

### Option 1: Starlight (Astro-based) -- RECOMMENDED

**URL**: https://starlight.astro.build/

**Pros**:
- Purpose-built for technical documentation sites
- Beautiful default theme with dark/light mode
- Built-in search (Pagefind -- local, no external service)
- Sidebar navigation auto-generated from file structure
- Excellent Mermaid diagram support via remark plugin
- Extremely fast (static site, Astro island architecture)
- Component-based: can embed interactive demos
- Internationalization support built in
- GitHub Pages deployment is trivial (Astro has official adapter)
- Growing rapidly in the open-source docs space (Astro, Biome, Clerk use it)

**Cons**:
- Requires Node.js for building (not Rust-native)
- Newer than alternatives (released 2023), smaller plugin ecosystem
- Astro learning curve if custom components needed

**Setup effort**: Low. `npm create astro@latest -- --template starlight` scaffolds everything. Markdown files drop in directly.

### Option 2: mdBook (Rust-native)

**URL**: https://rust-lang.github.io/mdBook/

**Pros**:
- Written in Rust (natural fit for a Rust project)
- Extremely simple: just Markdown + SUMMARY.md
- Used by official Rust documentation, Rust by Example, many Rust projects
- Fast build times, tiny output
- Built-in search
- Zero JavaScript framework dependency

**Cons**:
- Limited styling options -- looks like "a Rust project's docs" (which can be a pro or con)
- No built-in Mermaid support (requires preprocessor plugin)
- No dark mode toggle without custom CSS/JS
- Cannot embed interactive components
- No auto-generated sidebar from file structure (manual SUMMARY.md)
- Dated visual design compared to modern doc sites

**Setup effort**: Very low. `cargo install mdbook` + create `SUMMARY.md`.

### Option 3: Docusaurus (React-based)

**URL**: https://docusaurus.io/

**Pros**:
- Most mature documentation framework (Meta/Facebook)
- Extensive plugin ecosystem (search, diagrams, versioning)
- Built-in versioned docs (useful for v7 vs v8)
- Blog feature for announcements
- Large community, many examples
- Excellent Mermaid support via theme plugin

**Cons**:
- Heavy: React + Webpack build chain (~200MB node_modules)
- Slower builds than Starlight or mdBook
- Opinionated structure can be constraining
- React dependency feels heavy for a Rust project
- Slower iteration cycle during development

**Setup effort**: Medium. `npx create-docusaurus@latest` + configure `docusaurus.config.js`.

### Recommendation: Starlight

Starlight provides the best balance of:
1. **Professional appearance** -- modern, clean, competitive with top-tier open-source docs
2. **Developer experience** -- fast builds, Markdown-centric, minimal config
3. **Features needed** -- search, dark mode, Mermaid, sidebar, responsive
4. **Lightweight** -- much smaller than Docusaurus, more flexible than mdBook
5. **Community signal** -- using Starlight signals a modern, quality-conscious project

The visual polish alone could meaningfully impact adoption. The project competes with claude-mem (28.8K stars) -- documentation quality is a differentiator.

---

## Part 5: Branding Recommendations

### Name Treatment

**Primary name**: Claude Self-Reflect
**Short name / CLI name**: CSR or csr-engine
**Package name**: claude-self-reflect (npm)

**Recommended adjustment**: Consider whether "Self-Reflect" is the right name going forward. The current tagline "Claude forgets everything. This fixes that." is excellent and could be expanded into branding. However, the name is established and changing it has costs. Keep it.

### Tagline Options

Current: "Claude forgets everything. This fixes that."

This is strong -- direct, memorable, problem-solution in one line. Keep it as the primary tagline.

**Secondary taglines for different contexts**:
- Hero subtitle: "Cross-session memory for Claude Code. One binary, zero dependencies."
- Technical: "Semantic search over your Claude conversation history via MCP."
- Comparison: "claude-mem remembers. CSR makes Claude learn."

### Visual Identity

**Color palette** (derived from existing badges and project identity):
- Primary: `#6B4FBB` (Claude purple, from the existing badge)
- Accent: `#FF6B6B` (MCP red, from the existing badge)
- Neutral: `#1a1a2e` (dark background) / `#f5f5f5` (light background)
- Success: `#4A90E2` (local-first blue, from the existing badge)

**Typography**:
- Headings: Inter or system sans-serif (clean, professional)
- Code: JetBrains Mono or Fira Code
- Body: System font stack for speed

**Logo concept**:
- The project does not currently have a logo. Recommend creating one.
- Concept: A circular mirror/reflection motif combined with a search/memory icon
- Could be a stylized brain with a search magnifier, or two overlapping conversation bubbles forming a reflection
- Keep it simple enough to work as a favicon (16x16)

**Hero section concept**:
- Dark gradient background (#1a1a2e to #2a1a4e)
- Large tagline: "Claude forgets everything. This fixes that."
- Animated terminal showing `curl | sh` then `csr-engine setup` with realistic output
- Performance badges: "93ms startup" / "<1ms search" / "44MB binary"
- Two CTA buttons: "Get Started" and "View on GitHub"

### Content Voice

- **Authoritative but accessible**: This is a serious tool, not a toy
- **Terse over verbose**: Match the Rust ethos -- say more with less
- **Concrete over abstract**: Always show code, always show output
- **Honest about tradeoffs**: Don't hide limitations (e.g., macOS ARM only, model download on first run)

---

## Part 6: Content Migration Plan

### Phase 1: Foundation (Week 1)

**Goal**: Starlight site scaffolded, core pages written, deployed to GitHub Pages.

| Action | Source Material | Output |
|--------|----------------|--------|
| Scaffold Starlight project | -- | `docs-site/` directory |
| Write "What is CSR?" | `README.md`, `.internal/analysis/UNIQUE_VALUE.md`, `motivation-and-history.md` | New page |
| Write "Installation" | `README.md` (lines 17-38), current install flow | New page |
| Write "Quick Start" | `README.md`, `advanced-usage.md` concepts | New page |
| Write "System Overview" (architecture) | `README.md` diagram, `csr-engine/src/` module structure, `.plans/00-rust-rewrite-master-plan.md` | New page |
| Configure GitHub Pages deployment | -- | `.github/workflows/deploy-docs.yml` |

### Phase 2: Reference (Week 2)

**Goal**: Complete MCP tool reference and CLI reference.

| Action | Source Material | Output |
|--------|----------------|--------|
| Write MCP Tool Reference (12 tools) | `README.md` tool table, `api-reference.md` (2 tools), `csr-engine/src/mcp/tools.rs` source code, `development/MCP_REFERENCE.md` | New page, comprehensive |
| Write CLI Reference | `README.md` CLI table, `csr-engine/src/main.rs` (clap definitions) | New page |
| Write Configuration Reference | `.env` examples, `CLAUDE.md`, hook install output | New page |
| Write Hooks API Reference | `csr-engine/src/hooks/*.rs` source, stdin/stdout schemas | New page |

### Phase 3: Guides (Week 3)

**Goal**: User-facing guides for all major features.

| Action | Source Material | Output |
|--------|----------------|--------|
| Write "Hooks System" guide | `csr-engine/src/hooks/`, `CLAUDE.md` hooks section | **REWRITE** |
| Write "AI Narratives" guide | `CLAUDE.md` v7.0 section, `csr-engine/src/daemon/`, `data/SKILL_V2.md` | **REWRITE** |
| Write "Search Strategies" guide | `advanced-usage.md` concepts, `project-scoped-search.md` concepts | **ADAPT** |
| Write "Ralph Loop Memory" guide | `CLAUDE.md` Ralph section | **ADAPT** |
| Write "File Watcher" guide | `features/hot-warm-cold.md` concepts, `csr-engine/src/import/watcher.rs` | **ADAPT** |
| Update "Upgrading from v7.x" | `README.md` upgrading section | **ADAPT** |

### Phase 4: Architecture & Contributing (Week 4)

**Goal**: Deep technical content for contributors and advanced users.

| Action | Source Material | Output |
|--------|----------------|--------|
| Write full architecture docs | `csr-engine/src/` module structure, `.plans/` | **REWRITE** |
| Write "Memory Decay" | `memory-decay.md` concepts, `csr-engine/src/search/decay.rs` | **ADAPT** |
| Write "Contributing" guide | Current `CONTRIBUTING.md` structure, actual Rust workflow | **REWRITE** |
| Write "Design Decisions" | `.plans/00-rust-rewrite-master-plan.md`, competitive analysis concepts | New page |
| Write "Changelog" | All 57 release notes + Rust phase tracker | **CONSOLIDATE** |
| Update `SECURITY.md` | Current security.md concepts | **REWRITE** |

### Phase 5: Polish & Archive (Week 5)

**Goal**: Clean up old docs, add finishing touches.

| Action | Details |
|--------|---------|
| Archive old release notes | Move 57 files to `docs/archive/release-notes/` |
| Archive old testing reports | Move 35 files to `docs/archive/testing/` |
| Archive obsolete guides | Move Docker/Python-specific guides to `docs/archive/python-era/` |
| Add FAQ | Compile from troubleshooting, GitHub issues |
| Add Comparison page | From `.internal/` analysis (sanitized for public) |
| Create 404 page | Branded, with search |
| Set up redirects | Old doc URLs redirect to new locations |

### Content Disposition Summary

| Disposition | Count | Description |
|-------------|-------|-------------|
| **REWRITE** | ~12 | Core content exists but architecture/implementation details are wrong (Python/Docker references) |
| **ADAPT** | ~8 | Conceptual content is good, needs updated code examples and details |
| **NEW** | ~8 | No existing source material; must be written from source code |
| **CONSOLIDATE** | ~60 | Multiple files merged into one (release notes -> changelog) |
| **ARCHIVE** | ~100+ | Historical artifacts moved out of active docs |
| **DELETE** | ~30 | Redundant testing reports, duplicate release notes drafts |
| **KEEP AS-IS** | ~5 | Internal plans, research docs (not migrated to site) |

---

## Part 7: Example Page Outlines

### Page 1: Installation (Most Critical -- First Impression)

```
# Installation

> One command. Under a minute. No dependencies.

## Requirements
- macOS (Apple Silicon) or Linux (x86_64 / ARM64)
- Claude Code CLI installed and working

## Install

    curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh

What this does:
1. Downloads the ~44MB csr-engine binary for your platform
2. Places it in ~/.local/bin/ (or /usr/local/bin/)
3. Verifies the download

## Setup

    csr-engine setup

What this does:
1. Scans ~/.claude/projects/ for conversation JSONL files
2. Imports and indexes all conversations (~20/sec)
3. Registers the MCP server with Claude Code
4. Installs 6 Claude Code hooks
5. Shows a summary of what was imported

## Verify

    csr-engine status

Expected output:
[show realistic status output with conversation count, index stats, MCP status]

Restart Claude Code, then try:
"What did we work on recently?"
Claude will now search your conversation history.

## Alternative: npm install
[collapsible section with npm instructions]

## Platform Notes

### macOS (Apple Silicon) -- Primary
Fully supported. Native ARM64 binary.

### macOS (Intel) -- Not Supported
The ONNX Runtime used for embeddings does not provide Intel Mac binaries.
Workaround: Use Rosetta 2 (untested) or wait for upstream support.

### Linux x86_64
Fully supported. Tested on Ubuntu 22.04+.

### Linux ARM64
Supported. Tested on AWS Graviton instances.

### Windows
Use WSL2 with Ubuntu. See [Windows Setup Guide].

## Uninstall
[collapsible section with clean uninstall steps]

## Troubleshooting
[4-5 common install issues with solutions]

## Next Steps
- [Quick Start] -- Your first search in 60 seconds
- [MCP Tools Reference] -- What Claude can do with CSR
- [Hooks System] -- Real-time context injection
```

### Page 2: MCP Tools Reference (Most Requested -- Daily Use)

```
# MCP Tools Reference

CSR provides 12 MCP tools that Claude Code can use automatically.
Tools are available as soon as the MCP server is running.

## Search Tools

### csr_reflect_on_past
Semantic search across all past conversations.

**Parameters**
| Name | Type | Default | Description |
| query | string | required | Natural language search query |
| limit | integer | 5 | Max results (1-50) |
| project | string | auto | Project filter. "all" for cross-project. |
| brief | boolean | false | Compact output (fewer tokens) |
| use_decay | integer | -1 | Time decay: 1=on, 0=off, -1=default |

**Example usage by Claude**
[Show 3 examples: basic, with project filter, brief mode]

**Returns**
[Show annotated example response]

---

### csr_quick_check
Fast existence check. Returns count + top match.
[Same format: parameters table, examples, returns]

### search_by_recency
Time-constrained search.
[Parameters include time_range, since, until]

### get_recent_work
"What did we work on?" with session grouping.
[group_by parameter: day, session, project]

### get_timeline
Activity timeline with statistics.

### csr_search_by_file
Find conversations that touched a specific file.

### csr_search_by_concept
Theme-based search.

### csr_search_insights
Aggregated patterns from search results.

### csr_get_more
Pagination for additional results.

### get_full_conversation
Retrieve complete JSONL conversation by ID.

### get_session_learnings
Iteration-level memory for Ralph loops.

## Storage Tools

### store_reflection
Store insights for future retrieval.
[Parameters: content, tags, project]

## Tool Selection Guide

| I want to... | Use this tool |
|---------------|---------------|
| Find past conversations about a topic | csr_reflect_on_past |
| Check if we discussed something | csr_quick_check |
| See what I did this week | get_recent_work |
| Find conversations about a file | csr_search_by_file |
| Explore a concept across projects | csr_search_by_concept |
| Save an important decision | store_reflection |
```

### Page 3: System Architecture (Differentiator -- Shows Technical Depth)

```
# Architecture

CSR runs as a single 44MB binary. No containers, no network services,
no runtime dependencies.

## High-Level Architecture

[Mermaid diagram showing:]
~/.claude/projects/**/*.jsonl
    |
    v
+--[csr-engine]------------------------------------------+
|                                                          |
|  +----------+    +------------+    +---------+          |
|  | JSONL    | -> | FastEmbed  | -> | HNSW    |          |
|  | Parser   |    | (384-dim)  |    | Index   |          |
|  +----------+    +------------+    +---------+          |
|       |                                 |                |
|  +----------+                      +---------+          |
|  | SQLite   | <------------------> | Search  |          |
|  | Storage  |                      | Engine  |          |
|  +----------+                      +---------+          |
|       |                                 |                |
|  +----------+                      +---------+          |
|  | Enrichment|                     | MCP     |          |
|  | Pipeline  |                     | Server  |          |
|  +----------+                      +---------+          |
|                                         |                |
+------------------------------------------+               |
                                          |                |
                                    Claude Code
                                    (MCP Client)

## Component Deep Dive

### JSONL Parser (src/import/)
[How conversations are parsed, chunked, tool context extraction]

### Embedding Engine (src/embeddings/)
[FastEmbed, all-MiniLM-L6-v2, 384 dimensions, batch processing]
[Cache layer for model files]

### HNSW Search Index (src/search/)
[hnsw_rs, approximate nearest neighbors, persistence to disk]
[Cache-first loading: 93ms cached vs 14s rebuild]
[Staleness detection via IndexManifest]

### SQLite Storage (src/storage/)
[Schema: chunks, reflections, enrichment state, retrieval events]
[Migrations system]
[Thread safety: Mutex<Connection>]

### Search Engine (src/search/)
[HNSW search + decay scoring + cross-project resolver]
[Memory decay formula with explanation]
[Filtered search with adaptive over-fetch]

### MCP Server (src/mcp/)
[rmcp 0.15, 12 tools, Resources API]
[How tools map to search/storage operations]

### Enrichment Pipeline (src/extraction/, src/daemon/)
[3-layer system diagram]
Layer 1: Heuristic (inline, during import)
Layer 2: V3 Extraction (daemon, structured extraction)
Layer 3: AI Narrative (Anthropic Batch API, optional)
[Layer supersession: each layer replaces the previous]

### Hooks System (src/hooks/)
[6 hook types, dispatch architecture]
[stdin JSON -> processing -> stdout injection]
[catch-all error handling: never blocks Claude Code]

### Injection Engine (src/injection/)
[Token-budgeted formatting]
[Multi-signal predictor: semantic, recency, file_overlap, error_match]
[Stuck detection and anti-pattern injection]

## Data Flow Diagrams

### Import Flow
[Mermaid sequence diagram: file discovery -> parse -> chunk -> embed -> store -> index]

### Search Flow
[Mermaid sequence diagram: Claude query -> MCP tool -> embed query -> HNSW search -> decay score -> format -> return]

### Hook Flow
[Mermaid sequence diagram: Claude event -> hook dispatch -> engine search -> inject context]

## Design Decisions

### Why Rust?
[Single binary distribution, performance, no runtime deps]

### Why SQLite instead of Qdrant?
[Eliminated Docker dependency, simpler deployment, good enough for single-user]

### Why HNSW in-memory instead of sqlite-vec?
[sqlite-vec is KNN brute-force only, HNSW gives sub-millisecond ANN]

### Why FastEmbed instead of Voyage AI (default)?
[Privacy, no API key, offline capability, good enough accuracy]

## Performance Characteristics

| Operation | Typical Latency | Notes |
|-----------|----------------|-------|
| Cached startup | 93ms | Index loaded from disk |
| Cold startup | ~14s | Index rebuilt from DB |
| Search (p95) | <1ms | HNSW approximate nearest neighbor |
| Single embed | 2.5ms | FastEmbed all-MiniLM-L6-v2 |
| Batch embed (10) | 7.3ms | 0.73ms per text |
| Import rate | ~20 conv/sec | Batch embedding |
| Binary size | 44MB | Includes ONNX runtime |
| DB size | ~100MB | For 900+ conversations |

## File Structure

csr-engine/
  src/
    main.rs          -- CLI entry point (clap)
    engine.rs        -- Engine orchestrator
    lib.rs           -- Library root
    mcp/             -- MCP server + tools
    hooks/           -- 6 Claude Code hooks
    search/          -- HNSW + decay + cross-project
    storage/         -- SQLite + migrations + queries
    import/          -- JSONL parser + file watcher
    embeddings/      -- FastEmbed engine + cache
    extraction/      -- Enrichment pipeline (3 layers)
    injection/       -- Context injection engine
    format/          -- XML output formatters
    temporal/        -- Time parsing + grouping
    daemon/          -- Background enrichment daemon
    eval/            -- Evaluation framework
    api/             -- Types + sanitization
    setup.rs         -- One-shot setup command
    status.rs        -- System status reporting
    summarizer.rs    -- Text summarization
```

---

## Part 8: Implementation Checklist

### Prerequisites
- [ ] Choose domain: `docs.claude-self-reflect.dev` or use GitHub Pages default
- [ ] Create `docs-site/` directory in repo (or separate repo)
- [ ] Set up Starlight: `npm create astro@latest -- --template starlight`

### Week 1: Foundation
- [ ] Configure Starlight theme (colors, typography, logo placeholder)
- [ ] Write: What is CSR?
- [ ] Write: Installation
- [ ] Write: Quick Start
- [ ] Write: System Overview (architecture)
- [ ] Set up GitHub Pages deployment workflow
- [ ] Deploy initial site

### Week 2: Reference
- [ ] Write: MCP Tools Reference (all 12 tools from source code)
- [ ] Write: CLI Reference (from clap definitions in main.rs)
- [ ] Write: Configuration Reference
- [ ] Write: Hooks API Reference

### Week 3: Guides
- [ ] Write: Hooks System guide
- [ ] Write: AI Narratives guide
- [ ] Write: Search Strategies guide
- [ ] Write: Ralph Loop Memory guide
- [ ] Write: File Watcher guide
- [ ] Write: Upgrading from v7.x

### Week 4: Deep Content
- [ ] Write: Full architecture docs with Mermaid diagrams
- [ ] Write: Memory Decay explanation
- [ ] Write: Contributing guide (Rust)
- [ ] Write: Design Decisions
- [ ] Consolidate Changelog

### Week 5: Polish
- [ ] Archive old docs (100+ files)
- [ ] Write: FAQ
- [ ] Write: Comparison page
- [ ] Update root README.md to link to docs site
- [ ] Update CONTRIBUTING.md
- [ ] Update SECURITY.md
- [ ] Create logo / favicon
- [ ] SEO: meta descriptions, Open Graph tags
- [ ] Final review pass

### Ongoing
- [ ] Set up docs PR template (changes to csr-engine require docs update?)
- [ ] Add "Edit this page" links to GitHub
- [ ] Monitor search analytics for gaps
- [ ] Update docs with each release

---

## Part 9: Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Docs fall out of date again | HIGH | HIGH | Require docs update with code PRs. Add CI check for broken internal links. |
| Over-scoping the site | MEDIUM | MEDIUM | Start with Phase 1-2 only. Ship early, iterate. |
| Framework choice regret | LOW | MEDIUM | All three options use Markdown. Migration cost is mostly config, not content. |
| Old docs confuse users | HIGH | HIGH | Add deprecation banners to old docs immediately, even before new site launches. |
| Nobody reads the docs | MEDIUM | LOW | Good README + in-tool help (`csr-engine --help`) covers 80% of users. Docs are for the remaining 20%. |

---

## Appendix A: Files to Archive (Not Delete)

These files have historical value but confuse users if left in the active docs tree:

```
docs/testing/                          (entire directory -- 35 files)
docs/operations/RELEASE_NOTES_*        (12 files)
docs/RELEASE_NOTES_v*                  (30+ files)
docs/releases/                         (2 files)
docs/announcements/                    (7 files)
docs/development/v3.3.0-*              (6 files)
docs/development/v3.2.0-*              (1 file)
docs/architecture/streaming-*          (3 files)
docs/architecture/docker-*             (1 file)
docs/architecture/importcompact-*      (2 files)
docs/architecture/voyage-*             (1 file)
docs/PRODUCTION_CERTIFICATION_v4.0.md
docs/CORPORATE_MACHINE_TESTING.md
docs/docker-watcher-guide.md
docs/streaming-importer-architecture.md
docs/UNIFIED_STATE_MIGRATION_GUIDE.md
docs/development/STATE_CONSOLIDATION_SUMMARY.md
docs/development/importcompact-implementation.md
docs/development/packaging-workflow.md
docs/operations/importcompact-*        (2 files)
docs/operations/MCP_ASYNC_SUCCESS.md
docs/operations/CPU_OPTIMIZATION_ANALYSIS.md
```

**Target**: Move ~120 files to `docs/archive/`, reducing active docs from ~246 to ~30 curated pages.

## Appendix B: Immediate Quick Wins (Before Site Build)

These can be done right now to improve the documentation situation:

1. **Add deprecation notice to `docs/installation-guide.md`**: "This guide is for v7.x and earlier. For v8.0+, see the README."
2. **Update `CONTRIBUTING.md`**: Replace Docker/Node/Python setup with Rust toolchain instructions. This blocks contributors today.
3. **Update `SECURITY.md`**: Fix supported versions table to show v8.0.x.
4. **Update root `RELEASE_NOTES.md`**: Either delete it or make it point to the changelog.
5. **Add `csr-engine --help` output to README**: The CLI is self-documenting; surface it.
