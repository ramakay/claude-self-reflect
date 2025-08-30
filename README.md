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

🔒 **100% Local by Default** • ⚡ **Blazing Fast Search** • 🚀 **Zero Configuration** • 🏭 **Production Ready**

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

<details>
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

## ✨ The Magic

![Self Reflection vs The Grind](docs/images/red-reflection.webp)

## 📊 Before & After

![Before and After Claude Self-Reflect](docs/diagrams/before-after-combined.webp)

## 💬 Real Examples

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

## 🎯 Key Features

### Project-Scoped Search
Searches are **project-aware by default**. Claude automatically searches within your current project:

```
# In ~/projects/MyApp
You: "What authentication method did we use?"
Claude: [Searches ONLY MyApp conversations]

# To search everywhere
You: "Search all projects for WebSocket implementations"
Claude: [Searches across ALL your projects]
```

### ⏱️ Memory Decay
Recent conversations matter more. Old ones fade. Like your brain, but reliable.

### 🚀 Performance at Scale
- **Search**: <3ms average response time
- **Scale**: 600+ conversations across 24 projects
- **Reliability**: 100% indexing success rate
- **Memory**: 96% reduction from v2.5.15

## 🏗️ Architecture

![Import Architecture](docs/diagrams/import-architecture.png)

<details>
<summary>HOT/WARM/COLD Intelligent Prioritization</summary>

- **🔥 HOT** (< 5 minutes): 2-second intervals for near real-time import
- **🌡️ WARM** (< 24 hours): Normal priority with starvation prevention
- **❄️ COLD** (> 24 hours): Batch processed to prevent blocking

Files are categorized by age and processed with priority queuing to ensure newest content gets imported quickly while preventing older files from being starved.

</details>

## 🛠️ Requirements

- **Docker Desktop** (macOS/Windows) or **Docker Engine** (Linux)
- **Node.js** 16+ (for the setup wizard)
- **Claude Desktop** app

## 📖 Documentation

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

## 🆕 What's New

<details>
<summary>v2.8.0 - Latest Release</summary>

- **🔧 Fixed MCP Indexing**: Now correctly shows 97.1% progress (was showing 0%)
- **🔥 HOT/WARM/COLD**: Intelligent file prioritization for near real-time imports
- **📊 Enhanced Monitoring**: Real-time status with visual indicators

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

## 👥 Contributors

Special thanks to our contributors:
- **[@TheGordon](https://github.com/TheGordon)** - Fixed timestamp parsing (#10)
- **[@akamalov](https://github.com/akamalov)** - Ubuntu WSL insights
- **[@kylesnowschwartz](https://github.com/kylesnowschwartz)** - Security review (#6)

---

Built with ❤️ by [ramakay](https://github.com/ramakay) for the Claude community.