#!/bin/bash

# Claude Self-Reflect - Complete Uninstallation Script
# Removes all CSR components, data, and configurations

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Print colored output
print_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
print_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
print_warning() { echo -e "${YELLOW}[WARNING]${NC} $1"; }
print_error() { echo -e "${RED}[ERROR]${NC} $1"; }

echo -e "${RED}"
cat << "EOF"
 _   _       _           _        _ _
| | | |_ __ (_)_ __  ___| |_ __ _| | |
| | | | '_ \| | '_ \/ __| __/ _` | | |
| |_| | | | | | | | \__ \ || (_| | | |
 \___/|_| |_|_|_| |_|___/\__\__,_|_|_|

 Claude Self-Reflect
EOF
echo -e "${NC}"

# Ask for confirmation
echo ""
print_warning "This will remove ALL Claude Self-Reflect components:"
echo "  • Docker containers and volumes"
echo "  • Configuration files (~/.claude-self-reflect)"
echo "  • MCP server registration from Claude Desktop"
echo "  • Qdrant vector database and ALL stored conversations"
echo ""
read -p "Are you sure you want to continue? (yes/no) " -r
echo ""
if [[ ! $REPLY =~ ^[Yy][Ee][Ss]$ ]]; then
    print_info "Uninstall cancelled"
    exit 0
fi

# Option to keep conversation data
KEEP_DATA=false
read -p "Do you want to keep your conversation data for backup? (y/n) " -n 1 -r
echo ""
if [[ $REPLY =~ ^[Yy]$ ]]; then
    KEEP_DATA=true
    BACKUP_DIR="$HOME/claude-self-reflect-backup-$(date +%Y%m%d-%H%M%S)"
    print_info "Creating backup at $BACKUP_DIR"
    mkdir -p "$BACKUP_DIR"

    # Backup Qdrant data if running
    if docker ps | grep -q claude-reflection-qdrant; then
        print_info "Backing up Qdrant data..."
        docker run --rm \
            --volumes-from claude-reflection-qdrant \
            -v "$BACKUP_DIR:/backup" \
            alpine tar czf /backup/qdrant-data.tar.gz /qdrant/storage 2>/dev/null || true
    fi

    # Backup config files
    if [ -d "$HOME/.claude-self-reflect" ]; then
        cp -r "$HOME/.claude-self-reflect" "$BACKUP_DIR/config" 2>/dev/null || true
    fi

    print_success "Backup created at $BACKUP_DIR"
fi

# Stop and remove Docker containers
print_info "Stopping Docker containers..."
docker stop claude-reflection-qdrant 2>/dev/null || true
docker stop claude-reflection-safe-watcher 2>/dev/null || true
docker stop claude-reflection-streaming 2>/dev/null || true
docker stop claude-reflection-async 2>/dev/null || true
docker stop claude-reflection-watcher 2>/dev/null || true
docker stop claude-reflection-importer 2>/dev/null || true
docker stop claude-reflection-mcp 2>/dev/null || true

print_info "Removing Docker containers..."
docker rm claude-reflection-qdrant 2>/dev/null || true
docker rm claude-reflection-safe-watcher 2>/dev/null || true
docker rm claude-reflection-streaming 2>/dev/null || true
docker rm claude-reflection-async 2>/dev/null || true
docker rm claude-reflection-watcher 2>/dev/null || true
docker rm claude-reflection-importer 2>/dev/null || true
docker rm claude-reflection-mcp 2>/dev/null || true

# Remove Docker volumes unless keeping data
if [ "$KEEP_DATA" = false ]; then
    print_info "Removing Docker volumes..."
    docker volume rm claude-self-reflect_qdrant_data 2>/dev/null || true
fi

# Remove Docker network
print_info "Removing Docker network..."
docker network rm claude-reflection-network 2>/dev/null || true

# Remove Docker images
read -p "Remove Docker images? (saves disk space) (y/n) " -n 1 -r
echo ""
if [[ $REPLY =~ ^[Yy]$ ]]; then
    print_info "Removing Docker images..."
    docker rmi qdrant/qdrant:v1.15.1 2>/dev/null || true
    docker images | grep claude-reflection | awk '{print $3}' | xargs docker rmi -f 2>/dev/null || true
fi

# Remove configuration directory unless keeping data
if [ "$KEEP_DATA" = false ]; then
    print_info "Removing configuration directory..."
    rm -rf "$HOME/.claude-self-reflect"
else
    print_warning "Keeping configuration directory (backed up)"
fi

# Remove MCP server from Claude Desktop
print_info "Removing MCP server from Claude Desktop..."
CLAUDE_CONFIG="$HOME/Library/Application Support/Claude/claude_desktop_config.json"
if [ ! -f "$CLAUDE_CONFIG" ]; then
    CLAUDE_CONFIG="$HOME/.config/Claude/claude_desktop_config.json"
fi

if [ -f "$CLAUDE_CONFIG" ]; then
    # Backup current config
    cp "$CLAUDE_CONFIG" "$CLAUDE_CONFIG.backup-$(date +%Y%m%d-%H%M%S)"

    # Remove claude-self-reflect entry using Python
    python3 -c "
import json
import sys
try:
    with open('$CLAUDE_CONFIG', 'r') as f:
        config = json.load(f)
    if 'mcpServers' in config and 'claude-self-reflect' in config['mcpServers']:
        del config['mcpServers']['claude-self-reflect']
        with open('$CLAUDE_CONFIG', 'w') as f:
            json.dump(config, f, indent=2)
        print('Removed claude-self-reflect from MCP servers')
    else:
        print('claude-self-reflect not found in MCP servers')
except Exception as e:
    print(f'Error updating config: {e}', file=sys.stderr)
    sys.exit(1)
" && print_success "MCP server removed from Claude Desktop config" || print_warning "Could not update Claude Desktop config"
fi

# Remove local repository (if running from repo)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
if [ -d "$SCRIPT_DIR/.git" ]; then
    echo ""
    read -p "Remove local repository at $SCRIPT_DIR? (y/n) " -n 1 -r
    echo ""
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        cd "$HOME"
        print_info "Removing repository..."
        rm -rf "$SCRIPT_DIR"
    fi
fi

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
print_success "Claude Self-Reflect has been uninstalled! 🗑️"
echo ""

if [ "$KEEP_DATA" = true ]; then
    echo "📦 Your data backup is saved at:"
    echo "   $BACKUP_DIR"
    echo ""
fi

echo "📋 Next Steps:"
echo "  • Restart Claude Desktop to clear MCP server"
if [ "$KEEP_DATA" = true ]; then
    echo "  • To restore: reinstall and copy backup data back to ~/.claude-self-reflect"
fi
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
