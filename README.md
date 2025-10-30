# Claude Self-Reflect
<div align="center">
<img src="https://repobeats.axiom.co/api/embed/e45aa7276c6b2d1fbc46a9a3324e2231718787bb.svg" alt="Repobeats analytics image" />
</div>
<div align="center">

[![npm version](https://badge.fury.io/js/claude-self-reflect.svg)](https://www.npmjs.com/package/claude-self-reflect)
[![npm downloads](https://img.shields.io/npm/dm/claude-self-reflect.svg)](https://www.npmjs.com/package/claude-self-reflect)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![GitHub CI](https://github.com/ramakay/claude-self-reflect/actions/workflows/ci.yml/badge.svg)](https://github.com/ramakay/claude-self-reflect/actions/workflows/ci.yml)

[![Claude Code](https://img.shields.io/badge/Claude%20Code-Compatible-6B4FBB)](https://github.com/anthropics/claude-code)
[![MCP Protocol](https://img.shields.io/badge/MCP-Enabled-FF6B6B)](https://modelcontextprotocol.io/)
[![Docker](https://img.shields.io/badge/Docker-Ready-2496ED?logo=docker&logoColor=white)](https://www.docker.com/)
[![Local First](https://img.shields.io/badge/Local%20First-Privacy-4A90E2)](https://github.com/ramakay/claude-self-reflect)

[![GitHub stars](https://img.shields.io/github/stars/ramakay/claude-self-reflect.svg?style=social)](https://github.com/ramakay/claude-self-reflect/stargazers)
[![GitHub issues](https://img.shields.io/github/issues/ramakay/claude-self-reflect.svg)](https://github.com/ramakay/claude-self-reflect/issues)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](https://github.com/ramakay/claude-self-reflect/pulls)

</div>

**Claude forgets everything. This fixes that.**

Give Claude perfect memory of all your conversations. Search past discussions instantly. Never lose context again.

**100% Local by Default** • **20x Faster** • **Zero Configuration** • **Production Ready**

> **Latest: v7.0 Automated Narratives** - 9.3x better search quality via AI-powered summaries. [Learn more →](#v70-automated-narrative-generation)

## Why This Exists

Claude starts fresh every conversation. You've solved complex bugs, designed architectures, made critical decisions - all forgotten. Until now.

## Table of Contents

- [Quick Install](#quick-install)
- [Performance](#performance)
- [The Magic](#the-magic)
- [Before & After](#before--after)
- [Real Examples](#real-examples)
- [NEW: Real-time Indexing Status](#new-real-time-indexing-status-in-your-terminal)
- [Key Features](#key-features)
- [Code Quality Insights](#code-quality-insights)
- [Architecture](#architecture)
- [Requirements](#requirements)
- [Documentation](#documentation)
- [Keeping Up to Date](#keeping-up-to-date)
- [Troubleshooting](#troubleshooting)
- [Contributors](#contributors)

## Quick Install

```bash
# Install and run automatic setup (5 minutes, everything automatic)
npm install -g claude-self-reflect
claude-self-reflect setup

# That's it! The setup will:
# - Run everything in Docker (no Python issues!)
# - Configure everything automatically
# - Install the MCP in Claude Code
# - Start monitoring for new conversations
# - Keep all data local - no API keys needed
```

> [!TIP]
> **Auto-Migration**: Updates automatically handle breaking changes. Simply run `npm update -g claude-self-reflect`.

<details open>
<summary>Cloud Mode (Better Search Accuracy)</summary>

```bash
# Step 1: Get your free Voyage AI key
# Sign up at https://www.voyageai.com/ - it takes 30 seconds

# Step 2: Install with Voyage key
npm install -g claude-self-reflect
claude-self-reflect setup --voyage-key=YOUR_ACTUAL_KEY_HERE
```

> [!NOTE]
> Cloud mode provides 1024-dimensional embeddings (vs 384 local) for more accurate semantic search but sends conversation data to Voyage AI for processing.

</details>

## Performance

| Metric | Before | After | Improvement |
|--------|--------|-------|-------------|
| **Status Check** | 119ms | 6ms | **20x faster** |
| **Storage Usage** | 100MB | 50MB | **50% reduction** |
| **Import Speed** | 10/sec | 100/sec | **10x faster** |
| **Memory Usage** | 500MB | 50MB | **90% reduction** |
| **Search Latency** | 15ms | 3ms | **5x faster** |

### Competitive Comparison

| Feature | Claude Self-Reflect | MemGPT | LangChain Memory |
|---------|---------------------|---------|------------------|
| **Local-first** | Yes | No | Partial |
| **No API keys** | Yes | No | No |
| **Real-time indexing** | Yes 2-sec | Manual | No |
| **Search speed** | <3ms | ~50ms | ~100ms |
| **Setup time** | 5 min | 30+ min | 20+ min |
| **Docker required** | Yes | Python | Python |

## The Magic

![Self Reflection vs The Grind](docs/images/red-reflection.webp)

## Before & After

![Before and After Claude Self-Reflect](docs/diagrams/before-after-combined.webp)

## Real Examples

```
You: "How did we fix that 100% CPU usage bug?"
Claude: "Found it - we fixed the circular reference causing 100% CPU usage
        in the server modularization. Also fixed store_reflection dimension
        mismatch by creating separate reflections_local and reflections_voyage."

You: "What about that Docker memory issue?"
Claude: "The container was limited to 2GB but only using 266MB. We found
        the issue only happened with MAX_QUEUE_SIZE=1000 outside Docker.
        With proper Docker limits, memory stays stable at 341MB."

You: "Have we worked with JWT authentication?"
Claude: "Found conversations about JWT patterns including User.authenticate
        methods, TokenHandler classes, and concepts like token rotation,
        PKCE, and social login integration."
```

## NEW: Real-time Indexing Status in Your Terminal

See your conversation indexing progress directly in your statusline:

### Fully Indexed (100%)
![Statusline showing 100% indexed](docs/images/statusbar-1.png)

### Active Indexing (50% with backlog)
![Statusline showing 50% indexed with 7h backlog](docs/images/statusbar-2.png)

Works with [Claude Code Statusline](https://github.com/sirmalloc/ccstatusline) - shows progress bars, percentages, and indexing lag in real-time! The statusline also displays MCP connection status (✓ Connected) and collection counts (28/29 indexed).

## Code Quality Insights

<details>
<summary><b>AST-GREP Pattern Analysis (100+ Patterns)</b></summary>

### Real-time Quality Scoring in Statusline
Your code quality displayed live as you work:
- 🟢 **A+** (95-100): Exceptional code quality
- 🟢 **A** (90-95): Excellent, production-ready
- 🟢 **B** (80-90): Good, minor improvements possible
- 🟡 **C** (60-80): Fair, needs refactoring
- 🔴 **D** (40-60): Poor, significant issues
- 🔴 **F** (0-40): Critical problems detected

### Pattern Categories Analyzed
- **Security Patterns**: SQL injection, XSS vulnerabilities, hardcoded secrets
- **Performance Patterns**: N+1 queries, inefficient loops, memory leaks
- **Error Handling**: Bare exceptions, missing error boundaries
- **Type Safety**: Missing type hints, unsafe casts
- **Async Patterns**: Missing await, promise handling
- **Testing Patterns**: Test coverage, assertion quality

### How It Works
1. **During Import**: AST elements extracted from all code blocks
2. **Pattern Matching**: 100+ patterns from unified registry
3. **Quality Scoring**: Weighted scoring normalized by lines of code
4. **Statusline Display**: Real-time feedback as you code

</details>

## v7.0 Automated Narrative Generation

**9.3x Better Search Quality** • **50% Cost Savings** • **Fully Automated**

v7.0 introduces AI-powered conversation narratives that transform raw conversation excerpts into rich problem-solution summaries with comprehensive metadata extraction.

### Before/After Comparison

| Metric | v6.x (Raw Excerpts) | v7.0 (AI Narratives) | Improvement |
|--------|---------------------|----------------------|-------------|
| **Search Quality** | 0.074 | 0.691 | **9.3x better** |
| **Token Compression** | 100% | 18% | **82% reduction** |
| **Cost per Conversation** | $0.025 | $0.012 | **50% savings** |
| **Metadata Richness** | Basic | Tools + Concepts + Files | **Full context** |

### What You Get

**Enhanced Search Results:**
- **Problem-Solution Patterns**: Conversations structured as challenges encountered and solutions implemented
- **Rich Metadata**: Automatic extraction of tools used, technical concepts, and files modified
- **Context Compression**: 82% token reduction while maintaining searchability
- **Better Relevance**: Search scores improved from 0.074 to 0.691 (9.3x)

**Cost-Effective Processing:**
- Anthropic Batch API: $0.012 per conversation (vs $0.025 standard)
- Automatic batch queuing and processing
- Progress monitoring via Docker containers
- Evaluation generation for quality assurance

**Fully Automated Workflow:**
```bash
# 1. Watch for new conversations
docker compose up batch-watcher

# 2. Auto-trigger batch processing when threshold reached
# (Configurable: BATCH_THRESHOLD_FILES, default 10)

# 3. Monitor batch progress
docker compose logs batch-monitor -f

# 4. Enhanced narratives automatically imported to Qdrant
```

### Example: Raw Excerpt vs AI Narrative

**Before (v6.x)** - Raw excerpt showing basic conversation flow:
```
User: How do I fix the Docker memory issue?
Assistant: The container was limited to 2GB but only using 266MB...
```

**After (v7.0)** - Rich narrative with metadata:
```
PROBLEM: Docker container memory consumption investigation revealed
discrepancy between limits (2GB) and actual usage (266MB). Analysis
required to determine if memory limit was appropriate.

SOLUTION: Discovered issue occurred with MAX_QUEUE_SIZE=1000 outside
Docker environment. Implemented proper Docker resource constraints
stabilizing memory at 341MB.

TOOLS USED: Docker, grep, Edit
CONCEPTS: container-memory, resource-limits, queue-sizing
FILES: docker-compose.yaml, batch_watcher.py
```

### Getting Started with Narratives

Narratives are automatically generated for new conversations. To process existing conversations:

```bash
# Process all existing conversations in batch
python docs/design/batch_import_all_projects.py

# Monitor batch progress
docker compose logs batch-monitor -f

# Check completion status
curl http://localhost:6333/collections/csr_claude-self-reflect_local_384d
```

For complete documentation, see [Batch Automation Guide](docs/testing/NARRATIVE_TESTING_SUMMARY.md).

## Key Features

<details>
<summary><b>MCP Tools Available to Claude</b></summary>

**Search & Memory:**
- `reflect_on_past` - Search past conversations using semantic similarity with time decay (supports quick/summary modes)
- `store_reflection` - Store important insights or learnings for future reference
- `get_next_results` - Paginate through additional search results
- `search_by_file` - Find conversations that analyzed specific files
- `search_by_concept` - Search for conversations about development concepts
- `get_full_conversation` - Retrieve complete JSONL conversation files

**Temporal Queries:**
- `get_recent_work` - Answer "What did we work on last?" with session grouping
- `search_by_recency` - Time-constrained search like "docker issues last week"
- `get_timeline` - Activity timeline with statistics and patterns

**Runtime Configuration:**
- `switch_embedding_mode` - Switch between local/cloud modes without restart
- `get_embedding_mode` - Check current embedding configuration
- `reload_code` - Hot reload Python code changes
- `reload_status` - Check reload state
- `clear_module_cache` - Clear Python cache

**Status & Monitoring:**
- `get_status` - Real-time import progress and system status
- `get_health` - Comprehensive system health check
- `collection_status` - Check Qdrant collection health and stats

> [!TIP]
> Use `reflect_on_past --mode quick` for instant existence checks - returns count + top match only!

All tools are automatically available when the MCP server is connected to Claude Code.

</details>

<details>
<summary><b>Statusline Integration</b></summary>

See your indexing progress right in your terminal! Works with [Claude Code Statusline](https://github.com/sirmalloc/ccstatusline):
- **Progress Bar** - Visual indicator `[████████ ] 91%`
- **Indexing Lag** - Shows backlog `• 7h behind`
- **Auto-updates** every 60 seconds
- **Zero overhead** with intelligent caching

[Learn more about statusline integration →](docs/statusline-integration.md)

</details>

<details>
<summary><b>Project-Scoped Search</b></summary>

Searches are **project-aware by default**. Claude automatically searches within your current project:

```
# In ~/projects/MyApp
You: "What authentication method did we use?"
Claude: [Searches ONLY MyApp conversations]

# To search everywhere
You: "Search all projects for WebSocket implementations"
Claude: [Searches across ALL your projects]
```

</details>

<details>
<summary><b>Memory Decay</b></summary>

Recent conversations matter more. Old ones fade. Like your brain, but reliable.
- **90-day half-life**: Recent memories stay strong
- **Graceful aging**: Old information fades naturally
- **Configurable**: Adjust decay rate to your needs

> [!NOTE]
> Memory decay ensures recent solutions are prioritized while still maintaining historical context.

</details>

<details>
<summary><b>Performance at Scale</b></summary>

- **Search**: <3ms average response time
- **Scale**: 600+ conversations across 24 projects
- **Reliability**: 100% indexing success rate
- **Memory**: 96% reduction from v2.5.15
- **Real-time**: HOT/WARM/COLD intelligent prioritization

> [!TIP]
> For best performance, keep Docker allocated 4GB+ RAM and use SSD storage.

</details>

## Architecture

<details>
<summary><b>View Architecture Diagram & Details</b></summary>

![Import Architecture](docs/diagrams/import-architecture.png)

### HOT/WARM/COLD Intelligent Prioritization

- **HOT** (< 5 minutes): 2-second intervals for near real-time import
- **WARM** (< 24 hours): Normal priority with starvation prevention
- **COLD** (> 24 hours): Batch processed to prevent blocking

Files are categorized by age and processed with priority queuing to ensure newest content gets imported quickly while preventing older files from being starved.

### Components
- **Vector Database**: Qdrant for semantic search
- **MCP Server**: Python-based using FastMCP
- **Embeddings**: Local (FastEmbed) or Cloud (Voyage AI)
- **Import Pipeline**: Docker-based with automatic monitoring

</details>

## Requirements

<details>
<summary><b>System Requirements</b></summary>

### Minimum Requirements
- **Docker Desktop** (macOS/Windows) or **Docker Engine** (Linux)
- **Node.js** 16+ (for the setup wizard)
- **Claude Code** CLI
- **4GB RAM** available for Docker
- **2GB disk space** for vector database

### Recommended
- **8GB RAM** for optimal performance
- **SSD storage** for faster indexing
- **Docker Desktop 4.0+** for best compatibility

### Operating Systems
- macOS 11+ (Intel & Apple Silicon)
- Windows 10/11 with WSL2
- Linux (Ubuntu 20.04+, Debian 11+)

</details>

## Documentation

<details>
<summary>Technical Stack</summary>

- **Vector DB**: Qdrant (local, your data stays yours)
- **Embeddings**: 
  - Local (Default): FastEmbed with all-MiniLM-L6-v2
  - Cloud (Optional): Voyage AI
- **MCP Server**: Python + FastMCP
- **Search**: Semantic similarity with time decay

</details>

<details>
<summary>Advanced Topics</summary>

- [Performance tuning](docs/performance-guide.md)
- [Security & privacy](docs/security.md)
- [Windows setup](docs/windows-setup.md)
- [Architecture details](docs/architecture-details.md)
- [Contributing](CONTRIBUTING.md)

</details>

<details>
<summary>Troubleshooting</summary>

- [Troubleshooting Guide](docs/troubleshooting.md)
- [GitHub Issues](https://github.com/ramakay/claude-self-reflect/issues)
- [Discussions](https://github.com/ramakay/claude-self-reflect/discussions)

</details>

<details>
<summary>Uninstall</summary>

For complete uninstall instructions, see [docs/UNINSTALL.md](docs/UNINSTALL.md).

Quick uninstall:
```bash
# Remove MCP server
claude mcp remove claude-self-reflect

# Stop Docker containers
docker-compose down

# Uninstall npm package
npm uninstall -g claude-self-reflect
```

</details>

## Keeping Up to Date

```bash
npm update -g claude-self-reflect
```

Updates are automatic and preserve your data. See [full changelog](docs/release-history.md) for details.

<details>
<summary><b>Release Evolution</b></summary>

### v7.0 - Automated Narratives (Oct 2025)
- **9.3x better search quality** via AI-powered conversation summaries
- **50% cost savings** using Anthropic Batch API ($0.012 per conversation)
- **82% token compression** while maintaining searchability
- Rich metadata extraction (tools, concepts, files)
- Problem-solution narrative structure
- Automated batch processing with Docker monitoring

### v4.0 - Performance Revolution (Sep 2025)
- **20x faster** status checks (119ms → 6ms)
- **50% storage reduction** via unified state management
- **10x faster imports** (10/sec → 100/sec)
- **90% memory reduction** (500MB → 50MB)
- Runtime mode switching (no restart required)
- Prefixed collection naming (breaking change)
- Code quality tracking with AST-GREP (100+ patterns)

### v3.3 - Temporal Intelligence (Aug 2025)
- Time-based search: "docker issues last week"
- Session grouping: "What did we work on last?"
- Activity timelines with statistics
- Recency-aware queries

### v2.8 - Full Context Access (Jul 2025)
- Complete conversation retrieval
- JSONL file access for deeper analysis
- Enhanced debugging capabilities

[View complete changelog →](docs/release-history.md)

</details>

## Troubleshooting

<details>
<summary><b>Common Issues and Solutions</b></summary>

### 1. "No collections created" after import
**Symptom**: Import runs but Qdrant shows no collections  
**Cause**: Docker can't access Claude projects directory  
**Solution**:
```bash
# Run diagnostics to identify the issue
claude-self-reflect doctor

# Fix: Re-run setup to set correct paths
claude-self-reflect setup

# Verify .env has full paths (no ~):
cat .env | grep CLAUDE_LOGS_PATH
# Should show: CLAUDE_LOGS_PATH=/Users/YOUR_NAME/.claude/projects
```

### 2. MCP server shows "ERROR" but it's actually INFO
**Symptom**: `[ERROR] MCP server "claude-self-reflect" Server stderr: INFO Starting MCP server`  
**Cause**: Claude Code displays all stderr output as errors  
**Solution**: This is not an actual error - the MCP is working correctly. The INFO message confirms successful startup.

### 3. "No JSONL files found"
**Symptom**: Setup can't find any conversation files  
**Cause**: Claude Code hasn't been used yet or stores files elsewhere  
**Solution**:
```bash
# Check if files exist
ls ~/.claude/projects/

# If empty, use Claude Code to create some conversations first
# The watcher will import them automatically
```

### 4. Docker volume mount issues
**Symptom**: Import fails with permission errors  
**Cause**: Docker can't access home directory  
**Solution**:
```bash
# Ensure Docker has file sharing permissions
# macOS: Docker Desktop → Settings → Resources → File Sharing
# Add: /Users/YOUR_USERNAME/.claude

# Restart Docker and re-run setup
docker compose down
claude-self-reflect setup
```

### 5. Qdrant not accessible
**Symptom**: Can't connect to localhost:6333  
**Solution**:
```bash
# Start services
docker compose --profile mcp up -d

# Check if running
docker compose ps

# View logs for errors
docker compose logs qdrant
```

</details>

<details>
<summary><b>Diagnostic Tools</b></summary>

### Run Comprehensive Diagnostics
```bash
claude-self-reflect doctor
```

This checks:
- Docker installation and configuration
- Environment variables and paths
- Claude projects and JSONL files
- Import status and collections
- Service health

### Check Logs
```bash
# View all service logs
docker compose logs -f

# View specific service
docker compose logs qdrant
docker compose logs watcher
```

### Generate Diagnostic Report
```bash
# Create diagnostic file for issue reporting
claude-self-reflect doctor > diagnostic.txt
```

</details>

<details>
<summary><b>Getting Help</b></summary>

1. **Check Documentation**
   - [Troubleshooting Guide](docs/troubleshooting.md)
   - [FAQ](docs/faq.md)
   - [Windows Setup](docs/windows-setup.md)

2. **Community Support**
   - [GitHub Discussions](https://github.com/ramakay/claude-self-reflect/discussions)
   - [Discord Community](https://discord.gg/claude-self-reflect)

3. **Report Issues**
   - [GitHub Issues](https://github.com/ramakay/claude-self-reflect/issues)
   - Include diagnostic output when reporting

</details>

## Contributors

Special thanks to our contributors:
- **[@TheGordon](https://github.com/TheGordon)** - Fixed timestamp parsing (#10)
- **[@akamalov](https://github.com/akamalov)** - Ubuntu WSL insights
- **[@kylesnowschwartz](https://github.com/kylesnowschwartz)** - Security review (#6)

---

Built with care by [ramakay](https://github.com/ramakay) for the Claude community.