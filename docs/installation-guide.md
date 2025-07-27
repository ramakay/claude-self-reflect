# Claude Self-Reflect Installation Guide

This guide walks you through installing Claude Self-Reflect, a memory system that gives Claude the ability to search and reference past conversations.

## Table of Contents
- [Prerequisites](#prerequisites)
- [Installation Methods](#installation-methods)
- [Step-by-Step Installation](#step-by-step-installation)
- [Importing Conversations](#importing-conversations)
- [Configuring Claude Code](#configuring-claude-code)
- [Verifying Installation](#verifying-installation)
- [Troubleshooting](#troubleshooting)

## Prerequisites

Before installing Claude Self-Reflect, ensure you have:

1. **Claude Code** (Claude Desktop App)
   - Download from [Claude.ai](https://claude.ai)
   - Must be version that supports MCP (Model Context Protocol)

2. **Python 3.10 or higher**
   - Check version: `python3 --version`
   - Download from [python.org](https://python.org) if needed

3. **Docker Desktop**
   - Required for running Qdrant vector database
   - Download from [docker.com](https://docker.com)
   - **Important**: Docker daemon must be running

4. **Node.js 18+ and npm**
   - Check version: `node --version`
   - Download from [nodejs.org](https://nodejs.org) if needed

5. **Voyage AI API Key** (Optional - for Cloud mode only)
   - Sign up at [voyageai.com](https://www.voyageai.com/)
   - 200M free tokens per month
   - **Not required**: Local mode works without any API keys

## Choosing Your Embedding Mode

> [!IMPORTANT]
> Before installation, decide between Local or Cloud embeddings. This choice is semi-permanent - switching later requires re-importing all conversations (30+ minutes).

### 🔒 Local Embeddings (Default - Recommended)
- **Privacy**: All processing stays on your machine
- **Cost**: Free, no API usage
- **Offline**: Works without internet
- **Accuracy**: Good for most use cases
- **Model**: FastEmbed with all-MiniLM-L6-v2

### ☁️ Cloud Embeddings (Voyage AI)
- **Privacy**: Conversations sent to Voyage AI
- **Cost**: Free tier available (200M tokens/month)
- **Online**: Requires constant internet
- **Accuracy**: Best semantic search quality
- **Model**: voyage-3-large

**Choose Cloud mode only if**:
- Search accuracy is critical for your use case
- You're comfortable with external data processing
- You have reliable internet connectivity

For detailed migration instructions, see [Embedding Migration Guide](embedding-migration.md).

## Installation Methods

### Method 1: npm Install (Recommended)

The easiest way to install Claude Self-Reflect:

```bash
npm install -g claude-self-reflect
```

### Method 2: Clone from GitHub

For development or manual installation:

```bash
git clone https://github.com/ramakay/claude-self-reflect.git
cd claude-self-reflect
npm install
```

## Step-by-Step Installation

### 1. Run the Setup Wizard

After installation, run the interactive setup wizard:

```bash
claude-self-reflect setup
```

The wizard will:
- Check all prerequisites
- Start Qdrant database (if Docker is running)
- Set up Python environment
- Configure API keys
- Install Claude agents

### 2. Handle Docker/Qdrant Setup

When the wizard checks for Qdrant:

- If Docker is not running, you'll see:
  ```
  ❌ Docker is not running or not installed
  Please install Docker from https://docker.com and ensure the Docker daemon is running
  ```
  
  **Solution**: 
  1. Open Docker Desktop
  2. Wait for it to fully start (green icon)
  3. Run `claude-self-reflect setup` again

- If Docker is running, the wizard will ask:
  ```
  Would you like to start Qdrant with Docker? (y/n):
  ```
  
  Type `y` to automatically start Qdrant.

### 3. Configure Voyage AI

When prompted for your Voyage AI API key:

1. If you have one, paste it and press Enter
2. If you don't have one:
   - Press Enter to skip (you can add it later)
   - Get a free key from [voyageai.com](https://www.voyageai.com/)
   - Add it to the `.env` file later

### 4. Python Environment Setup

The wizard automatically:
- Creates a Python virtual environment
- Installs MCP server dependencies
- Installs import script dependencies

If you see any Python-related errors, ensure Python 3.10+ is installed and in your PATH.

## Importing Conversations

After setup is complete, import your existing Claude conversations:

### 1. Navigate to the Project Directory

```bash
cd claude-self-reflect
```

### 2. Activate the Python Environment

**macOS/Linux:**
```bash
source mcp-server/venv/bin/activate
```

**Windows:**
```bash
mcp-server\venv\Scripts\activate
```

### 3. Run the Import Script

```bash
python scripts/import-conversations-voyage.py
```

The script will:
- Find all Claude conversations in `~/.claude/projects`
- Create vector embeddings for each conversation
- Store them in Qdrant for semantic search

**Note**: First import may take a while depending on conversation history size.

### Import Options

For large conversation histories:

```bash
# Import with streaming (memory efficient)
python scripts/import-conversations-voyage-streaming.py

# Import only recent conversations
python scripts/import-conversations-voyage-streaming.py --limit 10
```

## Configuring Claude Code

### 1. Add the MCP Server

The setup wizard shows the exact command. It looks like:

```bash
claude mcp add claude-self-reflect "/path/to/claude-self-reflect/mcp-server/run-mcp.sh" \
  -e VOYAGE_KEY="your-voyage-api-key" \
  -e QDRANT_URL="http://localhost:6333"
```

### 2. Restart Claude Code

After adding the MCP server:
1. Completely quit Claude Code
2. Restart it
3. The reflection tools should now be available

### 3. Verify MCP Connection

In Claude Code, type:
```
claude mcp list
```

You should see `claude-self-reflect` in the list with status "Connected".

## Verifying Installation

### 1. Check Installation Status

```bash
claude-self-reflect doctor
```

This shows the status of:
- Python installation
- Qdrant connection
- MCP server files
- Environment configuration

### 2. Test Reflection Tools

In Claude Code, try:
- "What did we discuss about [topic]?"
- "Find our conversation about [subject]"
- "Remember this solution for next time"

If the reflection tools work, you'll see Claude searching past conversations.

## Troubleshooting

### Common Issues

#### "Docker is not running"
- **Solution**: Start Docker Desktop and wait for it to fully initialize

#### "VOYAGE_KEY environment variable not set"
- **Solution**: 
  1. Get API key from [voyageai.com](https://www.voyageai.com/)
  2. Add to `.env` file: `VOYAGE_KEY=your-key-here`

#### "Claude projects directory not found"
- **Solution**: 
  1. Open Claude Code
  2. Have at least one conversation
  3. Claude will create the `~/.claude/projects` directory

#### "Permission denied" errors
- **Solution**: 
  - On macOS/Linux: Use `sudo` for global npm install
  - Ensure you own the claude-self-reflect directory

#### Import script can't find dependencies
- **Solution**:
  1. Activate the virtual environment first
  2. Run: `pip install -r scripts/requirements.txt`

### Getting Help

- **Documentation**: [GitHub Wiki](https://github.com/ramakay/claude-self-reflect/wiki)
- **Issues**: [GitHub Issues](https://github.com/ramakay/claude-self-reflect/issues)
- **Discussions**: [GitHub Discussions](https://github.com/ramakay/claude-self-reflect/discussions)

## Next Steps

After successful installation:

1. **Regular Imports**: Set up automated imports for new conversations
2. **Customize Settings**: Adjust memory decay and search parameters
3. **Explore Agents**: Use specialized agents for different tasks

See [Advanced Usage](./advanced-usage.md) for more configuration options.