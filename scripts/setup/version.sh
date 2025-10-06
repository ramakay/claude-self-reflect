#!/bin/bash

# Claude Self-Reflect - Version Information Script
# Shows current version and checks for updates

set -e

# Colors
BLUE='\033[0;34m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

print_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
print_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
print_warning() { echo -e "${YELLOW}[WARNING]${NC} $1"; }

echo -e "${BLUE}"
cat << "EOF"
  ____ ____  ____   __     __            _
 / ___/ ___||  _ \  \ \   / /__ _ __ ___(_) ___  _ __
| |   \___ \| |_) |  \ \ / / _ \ '__/ __| |/ _ \| '_ \
| |___ ___) |  _ <    \ V /  __/ |  \__ \ | (_) | | | |
 \____|____/|_| \_\    \_/ \___|_|  |___/_|\___/|_| |_|
EOF
echo -e "${NC}"

# Get package version
PACKAGE_JSON="$(dirname "${BASH_SOURCE[0]}")/../../package.json"
if [ -f "$PACKAGE_JSON" ]; then
    VERSION=$(grep '"version"' "$PACKAGE_JSON" | head -1 | sed 's/.*"version": "\(.*\)".*/\1/')
    echo "📦 Claude Self-Reflect Version: ${GREEN}${VERSION}${NC}"
else
    print_warning "package.json not found - checking npm global install..."
    VERSION=$(npm list -g claude-self-reflect --depth=0 2>/dev/null | grep claude-self-reflect | sed 's/.*@\(.*\)/\1/' || echo "unknown")
    echo "📦 Claude Self-Reflect Version: ${GREEN}${VERSION}${NC}"
fi

echo ""

# Check Docker containers status
print_info "Docker Containers Status:"
echo ""
docker ps -a --filter "name=claude-reflection" --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}" 2>/dev/null || print_warning "No containers found"

echo ""

# Check for updates (optional)
if command -v npm &> /dev/null; then
    print_info "Checking for updates..."
    LATEST=$(npm show claude-self-reflect version 2>/dev/null || echo "unknown")
    if [ "$LATEST" != "unknown" ] && [ "$VERSION" != "$LATEST" ]; then
        print_warning "Update available: ${VERSION} → ${LATEST}"
        echo "  Run: npm install -g claude-self-reflect@latest"
    elif [ "$VERSION" = "$LATEST" ]; then
        print_success "You're on the latest version!"
    fi
fi

echo ""
echo "📚 Documentation: https://github.com/ram-senth/claude-self-reflect"
echo "🐛 Issues: https://github.com/ram-senth/claude-self-reflect/issues"
