#!/bin/bash

# Test npm package to ensure all files are included correctly
# This script tests the npm package in an isolated environment

set -e

echo "🧪 Testing npm package for claude-self-reflect"
echo "=============================================="

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Create a unique test directory
TEST_DIR="/tmp/test-claude-reflect-$$"
PACKAGE_NAME="claude-self-reflect"

echo -e "\n${YELLOW}📦 Creating test environment in $TEST_DIR${NC}"
mkdir -p "$TEST_DIR"

# Save current directory
ORIGINAL_DIR=$(pwd)

# Function to cleanup on exit
cleanup() {
    echo -e "\n${YELLOW}🧹 Cleaning up test environment${NC}"
    cd "$ORIGINAL_DIR"
    rm -rf "$TEST_DIR"
}
trap cleanup EXIT

# Step 1: Pack the current package
echo -e "\n${YELLOW}📦 Packing npm package...${NC}"
cd "$ORIGINAL_DIR"
npm pack --pack-destination="$TEST_DIR" > /dev/null 2>&1

# Get the package file name
PACKAGE_FILE=$(ls -1 "$TEST_DIR"/*.tgz | head -n1)
echo -e "   Created: $(basename "$PACKAGE_FILE")"

# Step 2: Install the package in test directory
echo -e "\n${YELLOW}📥 Installing package in test environment...${NC}"
cd "$TEST_DIR"
npm init -y > /dev/null 2>&1
npm install --save "$PACKAGE_FILE" > /dev/null 2>&1

# Step 3: Check if all required files exist
echo -e "\n${YELLOW}🔍 Verifying package contents...${NC}"

MISSING_FILES=()
FOUND_FILES=()

# Define files that should be in the package
REQUIRED_FILES=(
    "node_modules/$PACKAGE_NAME/installer/cli.js"
    "node_modules/$PACKAGE_NAME/installer/setup-wizard-docker.js"
    "node_modules/$PACKAGE_NAME/installer/setup-wizard.js"
    "node_modules/$PACKAGE_NAME/docker-compose.yaml"
    "node_modules/$PACKAGE_NAME/Dockerfile.importer"
    "node_modules/$PACKAGE_NAME/scripts/import-conversations-unified.py"
    "node_modules/$PACKAGE_NAME/scripts/delta-metadata-update-safe.py"
    "node_modules/$PACKAGE_NAME/mcp-server/src/server.py"
    "node_modules/$PACKAGE_NAME/mcp-server/run-mcp.sh"
)

for file in "${REQUIRED_FILES[@]}"; do
    if [ -f "$file" ]; then
        FOUND_FILES+=("$file")
        echo -e "   ${GREEN}✓${NC} $(basename "$file")"
    else
        MISSING_FILES+=("$file")
        echo -e "   ${RED}✗${NC} $(basename "$file") - MISSING!"
    fi
done

# Step 4: Test Docker build context
echo -e "\n${YELLOW}🐳 Checking Docker build context...${NC}"

# Check if docker-compose.yaml references exist
if [ -f "node_modules/$PACKAGE_NAME/docker-compose.yaml" ]; then
    # Check for volume mount issues
    if grep -q "./scripts:/scripts:ro" "node_modules/$PACKAGE_NAME/docker-compose.yaml"; then
        echo -e "   ${YELLOW}⚠️${NC}  docker-compose.yaml uses relative ./scripts mount"
        echo "      This will fail when running from npm global install"
    else
        echo -e "   ${GREEN}✓${NC} docker-compose.yaml volumes look correct"
    fi
    
    # Check if Dockerfile.importer copies scripts
    if [ -f "node_modules/$PACKAGE_NAME/Dockerfile.importer" ]; then
        if grep -q "COPY.*scripts" "node_modules/$PACKAGE_NAME/Dockerfile.importer"; then
            echo -e "   ${GREEN}✓${NC} Dockerfile.importer copies scripts"
        else
            echo -e "   ${RED}✗${NC} Dockerfile.importer does NOT copy scripts!"
            echo "      This is the root cause of issue #41"
        fi
    fi
fi

# Step 5: Test CLI accessibility
echo -e "\n${YELLOW}🖥️  Testing CLI command...${NC}"
if [ -x "node_modules/.bin/claude-self-reflect" ]; then
    echo -e "   ${GREEN}✓${NC} CLI command is executable"
    
    # Test help command (safe, won't modify anything)
    if node_modules/.bin/claude-self-reflect --help > /dev/null 2>&1; then
        echo -e "   ${GREEN}✓${NC} CLI help command works"
    else
        echo -e "   ${YELLOW}⚠️${NC}  CLI help command failed (might be expected)"
    fi
else
    echo -e "   ${RED}✗${NC} CLI command not found or not executable"
fi

# Step 6: Summary
echo -e "\n${YELLOW}📊 Test Summary${NC}"
echo "================================"
echo -e "Files found:    ${GREEN}${#FOUND_FILES[@]}${NC} / ${#REQUIRED_FILES[@]}"
echo -e "Files missing:  ${RED}${#MISSING_FILES[@]}${NC}"

if [ ${#MISSING_FILES[@]} -gt 0 ]; then
    echo -e "\n${RED}❌ Package test FAILED${NC}"
    echo "Missing files indicate the package won't work correctly when installed via npm"
    exit 1
else
    echo -e "\n${GREEN}✅ Package structure test PASSED${NC}"
    
    # Check for known issues
    if ! grep -q "COPY.*scripts" "node_modules/$PACKAGE_NAME/Dockerfile.importer" 2>/dev/null; then
        echo -e "\n${YELLOW}⚠️  WARNING: Issue #41 is still present${NC}"
        echo "   Dockerfile.importer needs to COPY scripts for npm package to work"
    fi
fi

echo -e "\n${GREEN}Test completed successfully!${NC}"