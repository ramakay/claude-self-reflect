# Uninstall Guide

## Complete Uninstall

Follow these steps to completely remove Claude Self-Reflect from your system.

### Step 1: Remove MCP Server from Claude Code
```bash
# Remove the MCP server configuration
claude mcp remove claude-self-reflect

# Verify removal
claude mcp list
```

### Step 2: Stop and Remove Docker Containers
```bash
# Stop all containers
docker-compose down

# Remove containers and volumes
docker-compose down -v

# Remove Docker images (optional)
docker rmi claude-self-reflect-streaming-importer
docker rmi claude-self-reflect-mcp-server
```

### Step 3: Remove npm Package
```bash
# Global uninstall
npm uninstall -g claude-self-reflect

# Verify removal
npm list -g claude-self-reflect
```

### Step 4: Remove Qdrant Data (Optional - This Deletes All Conversations!)
```bash
# WARNING: This permanently deletes all indexed conversations
docker stop qdrant
docker rm qdrant
docker volume rm claude-self-reflect_qdrant_storage

# Or if using local Qdrant
rm -rf ./data/qdrant_storage
```

### Step 5: Remove Project Files
```bash
# Remove the entire project directory
rm -rf ~/projects/claude-self-reflect

# Remove configuration files
rm -rf ~/.claude/agents/claude-self-reflect-*
rm -f ~/.env.claude-self-reflect
```

### Step 6: Remove Python Virtual Environment
```bash
# If you created a dedicated venv
rm -rf ~/claude-self-reflect-venv
```

### Step 7: Clean Claude Code Agent Cache
```bash
# Remove specialized sub-agents
rm -f ~/.claude/agents/qdrant-specialist.md
rm -f ~/.claude/agents/import-debugger.md
rm -f ~/.claude/agents/docker-orchestrator.md
rm -f ~/.claude/agents/mcp-integration.md
rm -f ~/.claude/agents/search-optimizer.md
rm -f ~/.claude/agents/reflection-specialist.md
```

## Partial Uninstall (Keep Data)

If you want to uninstall but preserve your indexed conversations:

```bash
# 1. Stop services but keep data
docker-compose stop

# 2. Remove MCP server
claude mcp remove claude-self-reflect

# 3. Uninstall npm package
npm uninstall -g claude-self-reflect

# Your data remains in Docker volumes and can be restored later
```

## Reinstall After Uninstall

To reinstall after a complete uninstall:
```bash
npm install -g claude-self-reflect
claude-self-reflect setup
```

To restore after a partial uninstall:
```bash
npm install -g claude-self-reflect
docker-compose up -d
claude mcp add claude-self-reflect "path/to/run-mcp.sh" -e QDRANT_URL="http://localhost:6333"
```