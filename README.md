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

## System Requirements

### Memory
- **Docker Memory**: 2GB minimum (4GB recommended for initial setup)
- **First Import**: May take 2-7 minutes to process all conversations
- **Subsequent Imports**: <60 seconds (only processes new/changed files)

💡 **First-Time User Note**: The initial import processes your entire conversation history. This is a one-time operation. After that, the system only imports new conversations, making it much faster and using less memory.

## Install

### Quick Start (Local Mode - Default)
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
# 🚀 Import watcher runs every 60 seconds
# ⚡ Memory decay enabled by default (90-day half-life)
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

### 🔒 Privacy & Data Exchange

| Mode | Data Storage | External API Calls | Data Sent | Search Quality |
|------|--------------|-------------------|-----------|----------------|
| **Local (Default)** | Your machine only | None | Nothing leaves your computer | Good - uses efficient local embeddings |
| **Cloud (Opt-in)** | Your machine | Voyage AI | Conversation text for embedding generation | Better - uses state-of-the-art models |

**Note**: Cloud mode sends conversation content to Voyage AI for processing. Review their [privacy policy](https://www.voyageai.com/privacy) before enabling.
 
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

## The Secret Sauce: Sub-Agents

Here's what makes this magical: **The Reflection Specialist sub-agent**.

When you ask about past conversations, Claude doesn't search in your main chat. Instead, it spawns a specialized sub-agent that:
- Searches your conversation history in its own context
- Brings back only the relevant results
- Keeps your main conversation clean and focused

**Your main context stays pristine**. No clutter. No token waste.

![Reflection Agent in Action](docs/images/Reflection-specialist.png)

## How It Works (10 Second Version)

Your conversations → Vector embeddings → Semantic search → Claude remembers

Technical details exist. You don't need them to start.

## Using It

Once installed, just talk naturally:

- "What did we discuss about database optimization?"
- "Find our debugging session from last week"
- "Remember this solution for next time"

The reflection specialist automatically activates. No special commands needed.

## Performance & Usage Guide

### 🚀 Lightning Fast Search
Optimized to deliver results in **200-350ms** (10-40x faster than v2.4.4)

### 🎯 Recommended Usage: Through Reflection-Specialist Agent

**Why use the agent instead of direct MCP tools?**
- **Preserves your main conversation context** - Search results don't clutter your working memory
- **Rich formatted responses** - Clean markdown instead of raw XML in your conversation
- **Better user experience** - Real-time streaming feedback and progress indicators
- **Proper tool counting** - Shows actual tool usage instead of "0 tool uses"
- **Automatic cross-project search** - Agent suggests searching across projects when relevant
- **Specialized search tools** - Access to quick_search, search_summary, and pagination

**Context Preservation Benefit:**
When you use the reflection-specialist agent, all the search results and processing happen in an isolated context. This means:
- Your main conversation stays clean and focused
- No XML dumps or raw data in your chat history
- Multiple searches won't exhaust your context window
- You get just the insights, not the implementation details

**Example:**
```
You: "What Docker issues did we solve?"
[Claude automatically spawns reflection-specialist agent]
⏺ reflection-specialist(Search Docker issues)
  ⎿ Searching 57 collections...
  ⎿ Found 5 relevant conversations
  ⎿ Done (1 tool use · 12k tokens · 2.3s)
[Returns clean, formatted insights without cluttering your context]
```

### ⚡ Performance Baselines

| Method | Search Time | Total Time | Context Impact | Best For |
|--------|------------|------------|----------------|----------|
| Direct MCP | 200-350ms | 200-350ms | Uses main context | Programmatic use, when context space matters |
| Via Agent | 200-350ms | 24-30s* | Isolated context | Interactive use, exploration, multiple searches |

*Note: The 24-30s includes context preservation overhead, which keeps your main conversation clean

**Note**: The specialized tools (`quick_search`, `search_summary`, `get_more_results`) only work through the reflection-specialist agent due to MCP protocol limitations.

## Key Features

### 🎯 Project-Scoped Search
Searches are **project-aware by default** (v2.4.3+). Claude automatically searches within your current project:

```
# In ~/projects/MyApp
You: "What authentication method did we use?"
Claude: [Searches ONLY MyApp conversations]

# To search everywhere
You: "Search all projects for WebSocket implementations"
Claude: [Searches across ALL your projects]
```

| Search Scope | How to Trigger | Example |
|------------|----------------|---------|
| Current Project (default) | Just ask normally | "What did we discuss about caching?" |
| All Projects | Say "all projects" | "Search all projects for error handling" |
| Specific Project | Name the project | "Find auth code in MyApp project" |

## Memory Decay

Recent conversations matter more. Old ones fade. Like your brain, but reliable.

Works perfectly out of the box. [Configure if you're particular](docs/memory-decay.md).

## For the Skeptics

**"Just use grep"** - Sure, enjoy your 10,000 matches for "database"  
**"Overengineered"** - Two functions: store_reflection, reflect_on_past  
**"Another vector DB"** - Yes, because semantic > string matching

Built by developers tired of re-explaining context every conversation.

## Requirements

- Claude Code or Claude Desktop
- Docker Desktop (macOS/Windows) or Docker Engine (Linux)
- Node.js 16+ (for the setup wizard only)
- 5 minutes for setup

## Upgrading from Earlier Versions

**v2.4.0+ includes major improvements:**
- **Docker-Only Setup**: No more Python environment issues!
- **Privacy First**: Local embeddings by default - your data never leaves your machine
- **Smarter Setup**: Handles existing installations gracefully
- **Better Security**: Automated vulnerability scanning
- **Real-Time Import**: Watcher checks for new conversations every 60 seconds
- **Fixed MCP Server**: Now uses correct server implementation with local embedding support

**To upgrade:**
```bash
npm update -g claude-self-reflect
claude-self-reflect setup  # Re-run setup, it handles everything
```

The setup wizard now detects and fixes common upgrade issues automatically. Your existing conversations remain searchable.

## Advanced Setup

Want to customize? See [Configuration Guide](docs/installation-guide.md).

## The Technical Stuff

If you must know:

- **Vector DB**: Qdrant (local, your data stays yours)
- **Embeddings**: 
  - Local (Default): FastEmbed with sentence-transformers/all-MiniLM-L6-v2
  - Cloud (Optional): Voyage AI (200M free tokens/month)
- **MCP Server**: Python + FastMCP
- **Search**: Semantic similarity with time decay

Both embedding options work well. Local mode uses FastEmbed for privacy and offline use. Cloud mode uses Voyage AI for enhanced accuracy when internet is available. We are not affiliated with Voyage AI.

### Want More Details?

- [Architecture Deep Dive](docs/architecture-details.md) - How it actually works
- [Components Guide](docs/components.md) - Each piece explained
- [Why We Built This](docs/motivation-and-history.md) - The full story
- [Advanced Usage](docs/advanced-usage.md) - Power user features

## Security

### Container Security Notice
⚠️ **Known Vulnerabilities**: Our Docker images are continuously monitored by Snyk and may show vulnerabilities in base system libraries. We want to be transparent about this:

- **Why they exist**: We use official Python Docker images based on Debian stable, which prioritizes stability over latest versions
- **Actual risk is minimal** because:
  - Most CVEs are in unused system libraries or require local access
  - Security patches are backported by Debian (version numbers don't reflect patches)
  - Our containers run as non-root users with minimal permissions
  - This is a local-only tool with no network exposure
- **What we're doing**: Regular updates, security monitoring, and evaluating alternative base images

**For production or security-sensitive environments**, consider:
- Building your own hardened images
- Running with additional security constraints (see below)
- Evaluating if the tool meets your security requirements

For maximum security:
```bash
# Run containers with read-only root filesystem
docker run --read-only --tmpfs /tmp claude-self-reflect
```

### Privacy & Data Security
- **Local by default**: Your conversations never leave your machine unless you explicitly enable cloud embeddings
- **No telemetry**: We don't track usage or collect any data
- **Secure storage**: All data stored in Docker volumes with proper permissions
- **API keys**: Stored in .env file with 600 permissions (read/write by owner only)

See our [Security Policy](SECURITY.md) for vulnerability reporting and more details.

## ⚠️ Important Disclaimers

### Tool Operation
- **Resource Usage**: The import process can be CPU and memory intensive, especially during initial import of large conversation histories
- **Data Processing**: This tool reads and indexes your Claude conversation files. Ensure you have adequate disk space
- **No Warranty**: This software is provided "AS IS" under the MIT License, without warranty of any kind
- **Data Responsibility**: You are responsible for your conversation data and any API keys used

### Limitations
- **Not Official**: This is a community tool, not officially supported by Anthropic
- **Experimental Features**: Some features like memory decay are experimental and may change
- **Import Delays**: Large conversation histories may take significant time to import initially
- **Docker Dependency**: Requires Docker to be running, which uses system resources

### Best Practices
- **Backup Your Data**: Always maintain backups of important conversations
- **Monitor Resources**: Check Docker resource usage if you experience system slowdowns
- **Test First**: Try with a small subset of conversations before full import
- **Review Logs**: Check import logs if conversations seem missing

By using this tool, you acknowledge these disclaimers and limitations.

## Problems?

- [Troubleshooting Guide](docs/troubleshooting.md)
- [GitHub Issues](https://github.com/ramakay/claude-self-reflect/issues)
- [Discussions](https://github.com/ramakay/claude-self-reflect/discussions)

## What's New

### Recent Updates
- **v2.4.5** - 10-40x performance boost, context preservation
- **v2.4.3** - Project-scoped search (breaking change) 
- **v2.3.7** - Local embeddings by default for privacy

📚 [Full Release History](docs/release-history.md) | 💬 [Discussions](https://github.com/ramakay/claude-self-reflect/discussions)

## Contributing

See our [Contributing Guide](CONTRIBUTING.md) for development setup and guidelines.

### Releasing New Versions (Maintainers)

Since our GitHub Actions automatically publish to npm, the release process is simple:

```bash
# 1. Ensure you're logged into GitHub CLI
gh auth login  # Only needed first time

# 2. Create and push a new tag
git tag v2.3.0  # Use appropriate version number
git push origin v2.3.0

# 3. Create GitHub release (this triggers npm publish)
gh release create v2.3.0 \
  --title "Release v2.3.0" \
  --notes-file CHANGELOG.md \
  --draft=false

# The GitHub Action will automatically:
# - Build the package
# - Run tests
# - Publish to npm
# - Update release assets
```

Monitor the release at: https://github.com/ramakay/claude-self-reflect/actions

---

Stop reading. Start installing. Your future self will thank you.

## Contributors & Acknowledgments

Special thanks to our contributors and security researchers:

- **[@TheGordon](https://github.com/TheGordon)** - Fixed critical timestamp parsing bug for Claude conversation exports (#10)
- **[@akamalov](https://github.com/akamalov)** - Highlighted Ubuntu WSL bug and helped educate about filesystem nuances
- **[@kylesnowschwartz](https://github.com/kylesnowschwartz)** - Comprehensive security review leading to v2.3.3+ security improvements (#6)

## Windows Configuration

### Recommended: Use WSL
For the best experience on Windows, we recommend using WSL (Windows Subsystem for Linux) which provides native Linux compatibility for Docker operations.

### Alternative: Native Windows
If using Docker Desktop on native Windows, you need to adjust the CONFIG_PATH in your `.env` file to use Docker-compatible paths:

```bash
# Replace USERNAME with your Windows username
CONFIG_PATH=/c/Users/USERNAME/.claude-self-reflect/config
```

This ensures Docker can properly mount the config directory. The setup wizard creates the directory, but Windows users need to update the path format for Docker compatibility.

MIT License. Built with ❤️ for the Claude community.