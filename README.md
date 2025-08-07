# Claude Self-Reflect

Claude forgets everything. This fixes that.

## What You Get

Ask Claude about past conversations. Get actual answers. **100% local by default** - your conversations never leave your machine. Cloud-enhanced search available when you need it.

**Before**: "I don't have access to previous conversations"  
**After**: 
```
⏺ reflection-specialist(Search FastEmbed vs cloud embedding decision)
  ⎿ Done (3 tool uses · 8.2k tokens · 12.4s)

"Found it! Yesterday we decided on FastEmbed for local mode - better privacy, 
no API calls, 384-dimensional embeddings. Works offline too."
```

The reflection specialist is a specialized sub-agent that Claude automatically spawns when you ask about past conversations. It searches your conversation history in its own isolated context, keeping your main chat clean and focused.

Your conversations become searchable. Your decisions stay remembered. Your context persists.

## Requirements

- **Docker Desktop** (macOS/Windows) or **Docker Engine** (Linux)
- **Node.js** 16+ (for the setup wizard)
- **Claude Desktop** app

## Quick Install

### Local Mode (Default - Your Data Stays Private)
```bash
# Install and run automatic setup
npm install -g claude-self-reflect
claude-self-reflect setup

# That's it! The setup will:
# ✅ Run everything in Docker (no Python issues!)
# ✅ Configure everything automatically
# ✅ Install the MCP in Claude Code  
# ✅ Start monitoring for new conversations
# ✅ Verify the reflection tools work
# 🔒 Keep all data local - no API keys needed
```

### Cloud Mode (Better Search Accuracy)
```bash
# Step 1: Get your free Voyage AI key
# Sign up at https://www.voyageai.com/ - it takes 30 seconds

# Step 2: Install with Voyage key
npm install -g claude-self-reflect
claude-self-reflect setup --voyage-key=YOUR_ACTUAL_KEY_HERE
```
*Note: Cloud mode provides more accurate semantic search but sends conversation data to Voyage AI for processing.*

5 minutes. Everything automatic. Just works.

## The Magic

![Self Reflection vs The Grind](docs/images/red-reflection.webp)

## Before & After

![Before and After Claude Self-Reflect](docs/diagrams/before-after-combined.webp)

## Real Examples That Made Us Build This

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

## How It Works

Your conversations → Vector embeddings → Semantic search → Claude remembers

Technical details exist. You don't need them to start.

## Import Architecture

Here's how your conversations get imported and prioritized:

![Import Architecture](docs/diagrams/import-architecture.png)

**The system intelligently prioritizes your conversations:**
- **🔥 HOT** (< 5 minutes): Switches to 2-second intervals for near real-time import
- **🌡️ WARM** (< 24 hours): Normal priority, processed every 60 seconds
- **❄️ COLD** (> 24 hours): Batch processed, max 5 per cycle to prevent blocking

## Using It

Once installed, just talk naturally:

- "What did we discuss about database optimization?"
- "Find our debugging session from last week"
- "Remember this solution for next time"

The reflection specialist automatically activates. No special commands needed.

## Key Features

### 🎯 Project-Scoped Search
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

### 🚀 Performance
- **Search**: 200-350ms response time
- **Import**: 2-second response for new conversations
- **Memory**: 50MB operational target with smart chunking

## The Technical Stack

- **Vector DB**: Qdrant (local, your data stays yours)
- **Embeddings**: 
  - Local (Default): FastEmbed with all-MiniLM-L6-v2
  - Cloud (Optional): Voyage AI
- **MCP Server**: Python + FastMCP
- **Search**: Semantic similarity with time decay

## Problems?

- [Troubleshooting Guide](docs/troubleshooting.md)
- [GitHub Issues](https://github.com/ramakay/claude-self-reflect/issues)
- [Discussions](https://github.com/ramakay/claude-self-reflect/discussions)

## What's New

- **v2.5.3** - Streamlined README & import architecture diagram
- **v2.5.2** - State file compatibility fix
- **v2.4.5** - 10-40x performance boost
- **v2.4.3** - Project-scoped search

[Full changelog](docs/release-history.md)

## Advanced Topics

- [Performance tuning](docs/performance-guide.md)
- [Security & privacy](docs/security.md)
- [Windows setup](docs/windows-setup.md)
- [Architecture details](docs/architecture-details.md)
- [Contributing](CONTRIBUTING.md)

## Contributors

Special thanks to our contributors:
- **[@TheGordon](https://github.com/TheGordon)** - Fixed timestamp parsing (#10)
- **[@akamalov](https://github.com/akamalov)** - Ubuntu WSL insights
- **[@kylesnowschwartz](https://github.com/kylesnowschwartz)** - Security review (#6)

---

Stop reading. Start installing. Your future self will thank you.

MIT License. Built with ❤️ for the Claude community.