#!/bin/bash

# Claude Self-Reflection MCP - Restore Script
# Restores a complete backup of your conversation memory

set -e

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Check if backup path is provided
if [ -z "$1" ]; then
    echo "Usage: ./restore.sh /path/to/backup/directory"
    exit 1
fi

BACKUP_DIR="$1"

if [ ! -d "$BACKUP_DIR" ]; then
    echo "Error: Backup directory not found: $BACKUP_DIR"
    exit 1
fi

if [ ! -f "$BACKUP_DIR/qdrant-data.tar.gz" ]; then
    echo "Error: Invalid backup - missing qdrant-data.tar.gz"
    exit 1
fi

echo -e "${BLUE}📥 Restoring from backup: $BACKUP_DIR${NC}"

# Show backup info
if [ -f "$BACKUP_DIR/backup-info.json" ]; then
    echo "Backup details:"
    jq . "$BACKUP_DIR/backup-info.json"
    echo ""
fi

# Confirm restore
read -p "This will overwrite all existing data. Continue? (y/N) " -n 1 -r
echo
if [[ ! $REPLY =~ ^[Yy]$ ]]; then
    echo "Restore cancelled."
    exit 0
fi

# Stop services
echo "• Stopping services..."
docker compose down

# Restore Qdrant data
echo "• Restoring Qdrant data..."
docker volume rm claude-self-reflect_qdrant_data 2>/dev/null || true
docker volume create claude-self-reflect_qdrant_data
docker run --rm -v claude-self-reflect_qdrant_data:/data -v "$BACKUP_DIR":/backup alpine tar -xzf /backup/qdrant-data.tar.gz -C /data

# Restore configuration
echo "• Restoring configuration..."
cp -r "$BACKUP_DIR/config" ./ 2>/dev/null || true
cp "$BACKUP_DIR/.env" ./ 2>/dev/null || echo "No .env file in backup"

# Restore import state
echo "• Restoring import state..."
cp -r "$BACKUP_DIR/config-isolated" ./ 2>/dev/null || true

# Start services
echo "• Starting services..."
docker compose up -d qdrant

# Wait for Qdrant to be ready
echo "• Waiting for Qdrant to initialize..."
sleep 5

# Verify restore
if curl -s http://localhost:6333/health | grep -q "ok"; then
    COLLECTIONS=$(curl -s http://localhost:6333/collections | jq -r '.result.collections | length')
    echo -e "${GREEN}✅ Restore complete!${NC}"
    echo "Restored $COLLECTIONS collections"
    echo ""
    echo "Next steps:"
    echo "1. Restart Claude Desktop to reconnect the MCP server"
    echo "2. Test search functionality"
else
    echo -e "${YELLOW}⚠️  Qdrant failed to start after restore${NC}"
    echo "Check logs with: docker compose logs qdrant"
fi