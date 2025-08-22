#!/bin/bash

# Test Docker builds and verify scripts are properly included
# This script tests Docker images in isolation without affecting running services

set -e

echo "🐳 Testing Docker builds for claude-self-reflect"
echo "================================================"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Test configuration
TEST_TAG="test-$$"
TEST_CONTAINER="test-claude-importer-$$"

# Function to cleanup on exit
cleanup() {
    echo -e "\n${YELLOW}🧹 Cleaning up test containers and images${NC}"
    docker rm -f "$TEST_CONTAINER" 2>/dev/null || true
    docker rmi "claude-self-reflect-importer:$TEST_TAG" 2>/dev/null || true
}
trap cleanup EXIT

# Step 1: Build test image
echo -e "\n${YELLOW}🔨 Building test Docker image...${NC}"
docker build -f Dockerfile.importer -t "claude-self-reflect-importer:$TEST_TAG" . > /dev/null 2>&1

if [ $? -eq 0 ]; then
    echo -e "   ${GREEN}✓${NC} Docker image built successfully"
else
    echo -e "   ${RED}✗${NC} Docker build failed!"
    exit 1
fi

# Step 2: Check if scripts exist in the image
echo -e "\n${YELLOW}🔍 Checking for scripts in Docker image...${NC}"

SCRIPTS_TO_CHECK=(
    "/app/scripts/delta-metadata-update-safe.py"
    "/app/scripts/import-conversations-unified.py"
    "/app/scripts/force-metadata-recovery.py"
    "/scripts/delta-metadata-update-safe.py"
    "/scripts/import-conversations-unified.py"
)

FOUND_SCRIPTS=()
MISSING_SCRIPTS=()

for script in "${SCRIPTS_TO_CHECK[@]}"; do
    # Run container and check if file exists
    if docker run --rm --name "$TEST_CONTAINER" "claude-self-reflect-importer:$TEST_TAG" \
        test -f "$script" 2>/dev/null; then
        FOUND_SCRIPTS+=("$script")
        echo -e "   ${GREEN}✓${NC} Found: $script"
    else
        MISSING_SCRIPTS+=("$script")
        echo -e "   ${RED}✗${NC} Missing: $script"
    fi
done

# Step 3: Test Python dependencies
echo -e "\n${YELLOW}🐍 Checking Python dependencies...${NC}"

PYTHON_DEPS=(
    "qdrant-client"
    "fastembed"
    "voyageai"
    "tqdm"
    "backoff"
    "tenacity"
)

for dep in "${PYTHON_DEPS[@]}"; do
    if docker run --rm "claude-self-reflect-importer:$TEST_TAG" \
        python -c "import $dep" 2>/dev/null; then
        echo -e "   ${GREEN}✓${NC} $dep is installed"
    else
        echo -e "   ${RED}✗${NC} $dep is NOT installed"
    fi
done

# Step 4: Test volume mount compatibility
echo -e "\n${YELLOW}📁 Testing volume mount scenarios...${NC}"

# Create temporary test directory
TEST_DIR="/tmp/docker-test-$$"
mkdir -p "$TEST_DIR/scripts"
echo "print('Test script executed')" > "$TEST_DIR/scripts/test.py"

# Test with volume mount (simulating local development)
if docker run --rm -v "$TEST_DIR/scripts:/scripts:ro" \
    "claude-self-reflect-importer:$TEST_TAG" \
    python /scripts/test.py 2>/dev/null | grep -q "Test script executed"; then
    echo -e "   ${GREEN}✓${NC} Volume mount works (local development mode)"
else
    echo -e "   ${YELLOW}⚠️${NC}  Volume mount test failed"
fi

# Test without volume mount (simulating npm package)
if docker run --rm "claude-self-reflect-importer:$TEST_TAG" \
    test -f /app/scripts/delta-metadata-update-safe.py 2>/dev/null; then
    echo -e "   ${GREEN}✓${NC} Scripts available without volume mount (npm package mode)"
else
    echo -e "   ${RED}✗${NC} Scripts NOT available without volume mount!"
    echo "      This confirms issue #41"
fi

# Cleanup test directory
rm -rf "$TEST_DIR"

# Step 5: Check Dockerfile for COPY instruction
echo -e "\n${YELLOW}📄 Analyzing Dockerfile.importer...${NC}"

if grep -q "COPY.*scripts" Dockerfile.importer; then
    echo -e "   ${GREEN}✓${NC} Dockerfile has COPY instruction for scripts"
else
    echo -e "   ${RED}✗${NC} Dockerfile is missing COPY instruction for scripts!"
    echo "      Add: COPY scripts/ /app/scripts/"
fi

# Step 6: Test with docker-compose
echo -e "\n${YELLOW}🐋 Testing docker-compose configuration...${NC}"

# Check if scripts directory would be accessible
if [ -d "./scripts" ]; then
    echo -e "   ${GREEN}✓${NC} Local scripts directory exists"
else
    echo -e "   ${RED}✗${NC} Local scripts directory not found"
fi

# Check docker-compose.yaml for volume mounts
if grep -q "./scripts:/scripts:ro" docker-compose.yaml; then
    echo -e "   ${YELLOW}⚠️${NC}  docker-compose uses relative path ./scripts"
    echo "      This won't work with npm global install"
fi

# Step 7: Summary
echo -e "\n${YELLOW}📊 Test Summary${NC}"
echo "================================"

if [ ${#MISSING_SCRIPTS[@]} -eq 0 ]; then
    echo -e "${GREEN}✅ All scripts found in Docker image${NC}"
else
    echo -e "${RED}❌ Scripts missing in Docker image${NC}"
    echo "Missing scripts: ${#MISSING_SCRIPTS[@]}"
    echo ""
    echo "To fix issue #41, add to Dockerfile.importer:"
    echo "  COPY scripts/ /app/scripts/"
    echo ""
    echo "This ensures scripts are available when:"
    echo "  1. Running from npm global install"
    echo "  2. Volume mount is not available"
fi

echo -e "\n${GREEN}Docker build test completed!${NC}"