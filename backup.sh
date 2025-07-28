#!/bin/bash

# Claude Self-Reflection MCP - Backup Script
# Creates a complete backup of your conversation memory

set -e

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m'

# Check if backup path is provided
if [ -z "$1" ]; then
    echo "Usage: ./backup.sh /path/to/backup/directory"
    exit 1
fi

BACKUP_DIR="$1/claude-self-reflection-backup-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$BACKUP_DIR"

echo -e "${BLUE}📦 Creating backup in: $BACKUP_DIR${NC}"

# Backup Qdrant data
echo "• Backing up Qdrant data..."
docker run --rm -v claude-self-reflect_qdrant_data:/data -v "$BACKUP_DIR":/backup alpine tar -czf /backup/qdrant-data.tar.gz -C /data .

# Backup configuration
echo "• Backing up configuration..."
cp -r config "$BACKUP_DIR/"
cp .env "$BACKUP_DIR/" 2>/dev/null || echo "No .env file found"

# Backup import state
echo "• Backing up import state..."
cp -r config-isolated "$BACKUP_DIR/" 2>/dev/null || true

# Create backup metadata
echo "• Creating backup metadata..."
cat > "$BACKUP_DIR/backup-info.json" << EOF
{
  "timestamp": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")",
  "version": "$(git describe --tags --always 2>/dev/null || echo "unknown")",
  "collections": $(curl -s http://localhost:6333/collections | jq -r '.result.collections | length' || echo 0),
  "total_vectors": $(python scripts/check-collections.py 2>/dev/null | grep "Total vectors" | awk '{print $3}' || echo 0)
}
EOF

echo -e "${GREEN}✅ Backup complete!${NC}"
echo "Backup location: $BACKUP_DIR"
echo ""
echo "To restore from this backup, run:"
echo "./restore.sh $BACKUP_DIR"