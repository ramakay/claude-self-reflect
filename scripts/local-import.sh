#!/bin/bash
# Local import script that uses system Python with virtual environment

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
VENV_DIR="$PROJECT_ROOT/.venv"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}Claude Self-Reflection - Local Import Tool${NC}"
echo "============================================"

# Check if virtual environment exists
if [ ! -d "$VENV_DIR" ]; then
    echo -e "${YELLOW}Creating Python virtual environment...${NC}"
    python3 -m venv "$VENV_DIR"
fi

# Activate virtual environment
source "$VENV_DIR/bin/activate"

# Install dependencies if needed
if ! python -c "import qdrant_client" 2>/dev/null; then
    echo -e "${YELLOW}Installing dependencies...${NC}"
    pip install --upgrade pip
    pip install \
        qdrant-client \
        sentence-transformers \
        numpy \
        psutil \
        torch --index-url https://download.pytorch.org/whl/cpu
fi

# Set environment variables
export QDRANT_URL="http://localhost:6333"
export STATE_FILE="$PROJECT_ROOT/config-isolated/imported-files.json"
export EMBEDDING_MODEL="sentence-transformers/all-MiniLM-L6-v2"
export BATCH_SIZE="20"
export MAX_MEMORY_MB="400"
export PYTHONUNBUFFERED=1

# Check if Qdrant is running
if ! curl -s "$QDRANT_URL/collections" > /dev/null; then
    echo -e "${RED}Error: Qdrant is not running at $QDRANT_URL${NC}"
    echo "Please start Qdrant with: docker compose up -d qdrant"
    exit 1
fi

# Run the import
if [ $# -eq 0 ]; then
    echo -e "${GREEN}Starting streaming import of all Claude projects...${NC}"
    echo -e "${YELLOW}Using low-memory streaming mode (50MB limit)${NC}"
    python "$SCRIPT_DIR/streaming-importer.py"
else
    echo -e "${GREEN}Importing specific project: $1${NC}"
    echo -e "${YELLOW}Using low-memory streaming mode (50MB limit)${NC}"
    python "$SCRIPT_DIR/streaming-importer.py" "$1"
fi

# Show summary
echo ""
echo -e "${GREEN}Import Summary:${NC}"
curl -s "$QDRANT_URL/collections" | \
    jq -r '.result.collections[] | select(.name | startswith("conv_")) | "\(.name): \(.points_count // 0) points"' | \
    sort

# Deactivate virtual environment
deactivate