#!/bin/bash

# CI/CD Validation: Docker Compose Configuration
# Ensures docker-compose.yaml doesn't contain paths that will fail in production

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

print_error() { echo -e "${RED}[ERROR]${NC} $1"; }
print_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
print_warning() { echo -e "${YELLOW}[WARNING]${NC} $1"; }

DOCKER_COMPOSE_FILE="docker-compose.yaml"
FAILED=0

echo "🔍 Validating Docker Compose Configuration..."
echo ""

# Check 1: No relative path mounts (./src, ./shared, etc.)
echo "Checking for invalid relative path mounts..."
if grep -E "^\s*-\s+\./src:" "$DOCKER_COMPOSE_FILE" > /dev/null 2>&1; then
    print_error "Found invalid mount: './src:/app/src' - This will fail in npm global installs!"
    echo "  Reason: /opt/homebrew/lib/ is not a Docker-allowed path on macOS"
    echo "  Fix: Remove the mount - code should be in Docker image already"
    FAILED=1
fi

if grep -E "^\s*-\s+\./shared:" "$DOCKER_COMPOSE_FILE" > /dev/null 2>&1; then
    print_error "Found invalid mount: './shared:/app/shared' - This will fail in npm global installs!"
    echo "  Reason: /opt/homebrew/lib/ is not a Docker-allowed path on macOS"
    echo "  Fix: Remove the mount - code should be in Docker image already"
    FAILED=1
fi

# Check 2: Validate environment variable mounts use correct syntax
echo "Checking environment variable mounts..."
INVALID_MOUNTS=$(grep -E "^\s*-\s+\\\$\{[A-Z_]+\}:" "$DOCKER_COMPOSE_FILE" || true)
if [ -n "$INVALID_MOUNTS" ]; then
    print_error "Found invalid mount syntax: \${VAR}:/path"
    echo "  Use: \${VAR:-~/default}:/path (with default value)"
    echo "$INVALID_MOUNTS"
    FAILED=1
fi

# Check 3: Ensure CONFIG_PATH has proper defaults
echo "Checking CONFIG_PATH usage..."
if ! grep -E "CONFIG_PATH:-~/\.claude-self-reflect/config" "$DOCKER_COMPOSE_FILE" > /dev/null; then
    print_warning "CONFIG_PATH should have default: \${CONFIG_PATH:-~/.claude-self-reflect/config}"
fi

# Check 4: Ensure CLAUDE_LOGS_PATH has proper defaults
echo "Checking CLAUDE_LOGS_PATH usage..."
if ! grep -E "CLAUDE_LOGS_PATH:-~/\.claude/projects" "$DOCKER_COMPOSE_FILE" > /dev/null; then
    print_warning "CLAUDE_LOGS_PATH should have default: \${CLAUDE_LOGS_PATH:-~/.claude/projects}"
fi

# Check 5: No absolute paths outside allowed Docker directories (macOS)
echo "Checking for macOS Docker path restrictions..."
RESTRICTED_PATHS=$(grep -E "^\s*-\s+/(opt|usr/local|Library)/" "$DOCKER_COMPOSE_FILE" | grep -v "/tmp" || true)
if [ -n "$RESTRICTED_PATHS" ]; then
    print_error "Found mounts to restricted paths (not allowed by Docker Desktop on macOS):"
    echo "$RESTRICTED_PATHS"
    echo "  Allowed: /Users, /Volumes, /private, /tmp"
    echo "  Fix: Use \${VAR:-~/.local/path} syntax to mount from user home"
    FAILED=1
fi

echo ""
if [ $FAILED -eq 0 ]; then
    print_success "✅ Docker Compose validation passed!"
    exit 0
else
    print_error "❌ Docker Compose validation failed!"
    echo ""
    echo "Common fixes:"
    echo "  1. Remove ./src and ./shared mounts (code is in Docker images)"
    echo "  2. Use environment variables with defaults: \${VAR:-~/path}"
    echo "  3. Keep mounts within Docker-allowed directories"
    exit 1
fi
