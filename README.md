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

**100% Local by Default** • **Blazing Fast Search** • **Zero Configuration** • **Production Ready**

## Table of Contents

- [Quick Install](#-quick-install)
- [The Magic](#the-magic)
- [Before & After](#before--after)
- [Real Examples](#real-examples)
- [NEW: Real-time Indexing Status](#new-real-time-indexing-status-in-your-terminal)
- [Key Features](#key-features)
- [Architecture](#architecture)
- [Requirements](#requirements)
- [Documentation](#documentation)
- [What's New](#whats-new)
- [Troubleshooting](#troubleshooting)
- [Contributors](#contributors)

## 🚀 Quick Install

```bash
# Install and run automatic setup (5 minutes, everything automatic)
npm install -g claude-self-reflect
claude-self-reflect setup

# That's it! The setup will:
# ✅ Run everything in Docker (no Python issues!)
# ✅ Configure everything automatically
# ✅ Install the MCP in Claude Code  
# ✅ Start monitoring for new conversations
# 🔒 Keep all data local - no API keys needed
```

<details open>
<summary>📡 Cloud Mode (Better Search Accuracy)</summary>

```bash
# Step 1: Get your free Voyage AI key
# Sign up at https://www.voyageai.com/ - it takes 30 seconds

# Step 2: Install with Voyage key
npm install -g claude-self-reflect
claude-self-reflect setup --voyage-key=YOUR_ACTUAL_KEY_HERE
```
*Note: Cloud mode provides more accurate semantic search but sends conversation data to Voyage AI for processing.*

</details>

## The Magic

![Self Reflection vs The Grind](docs/images/red-reflection.webp)

## Before & After

![Before and After Claude Self-Reflect](docs/diagrams/before-after-combined.webp)

## Real Examples

```
You: "What was that PostgreSQL optimization we figured out?"
Claude: "Found it - conversation from Dec 15th. You discovered that adding 
        a GIN index on the metadata JSONB column reduced query time from 
        2.3s to 45ms."

You: "Remember that React hooks bug?"
Claude: "Yes, from last week. The useEffect was missing a dependency on 
        userId, causing stale closures in the event handler."

You: "Have we discussed WebSocket authentication before?"
Claude: "3 conversations found:
        - Oct 12: Implemented JWT handshake for Socket.io
        - Nov 3: Solved reconnection auth with refresh tokens  
        - Nov 20: Added rate limiting per authenticated connection"
```

## NEW: Real-time Indexing Status in Your Terminal

See your conversation indexing progress directly in your statusline:

### Fully Indexed (100%)
![Statusline showing 100% indexed](docs/images/statusbar-1.png)

### Active Indexing (50% with backlog)
![Statusline showing 50% indexed with 7h backlog](docs/images/statusbar-2.png)

Works with [Claude Code Statusline](https://github.com/sirmalloc/ccstatusline) - shows progress bars, percentages, and indexing lag in real-time!

## Key Features

<details>
<summary><b>MCP Tools Available to Claude</b></summary>

**Search & Memory Tools:**
- `reflect_on_past` - Search past conversations using semantic similarity with time decay
- `store_reflection` - Store important insights or learnings for future reference
- `quick_search` - Fast search returning only count and top result
- `search_summary` - Get aggregated insights without individual details
- `get_more_results` - Paginate through additional search results
- `search_by_file` - Find conversations that analyzed specific files
- `search_by_concept` - Search for conversations about development concepts
- `get_full_conversation` - Retrieve complete JSONL conversation files (v2.8.8)

**Status & Monitoring Tools:**
- `get_status` - Real-time import progress and system status
- `get_health` - Comprehensive system health check
- `collection_status` - Check Qdrant collection health and stats

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

</details>

<details>
<summary><b>Performance at Scale</b></summary>

- **Search**: <3ms average response time
- **Scale**: 600+ conversations across 24 projects
- **Reliability**: 100% indexing success rate
- **Memory**: 96% reduction from v2.5.15
- **Real-time**: HOT/WARM/COLD intelligent prioritization

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

## What's New

<details>
<summary>v2.8.8 - Latest Release</summary>

- **Full Conversation Access**: New `get_full_conversation` tool provides complete JSONL files instead of 200-char excerpts
- **95% Value Increase**: Agents can now access entire conversations with full implementation details
- **Direct File Access**: Returns absolute paths for efficient reading with standard tools

</details>

<details>
<summary>v2.5.19 - Metadata Enrichment</summary>

### For Existing Users
```bash
# Update to latest version
npm update -g claude-self-reflect

# Run setup - it will detect your existing installation
claude-self-reflect setup
# Choose "yes" when asked about metadata enrichment

# Or manually enrich metadata anytime:
docker compose run --rm importer python /app/scripts/delta-metadata-update-safe.py
```

### What You Get
- `search_by_concept("docker")` - Find conversations by topic
- `search_by_file("server.py")` - Find conversations that touched specific files
- Better search accuracy with metadata-based filtering

</details>

<details>
<summary>Release History</summary>

- **v2.5.18** - Security dependency updates
- **v2.5.17** - Critical CPU fix and memory limit adjustment
- **v2.5.16** - Initial streaming importer with CPU throttling
- **v2.5.15** - Critical bug fixes and collection creation improvements
- **v2.5.14** - Async importer collection fix
- **v2.5.11** - Critical cloud mode fix
- **v2.5.10** - Emergency hotfix for MCP server startup
- **v2.5.6** - Tool Output Extraction

[Full changelog](docs/release-history.md)

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