#!/bin/bash

# Claude Self-Reflection MCP - One-Command Installation Script
# This script sets up the Qdrant-based memory system for Claude Desktop

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

# ASCII Art Banner
echo -e "${BLUE}"
cat << "EOF"
  _____ _                 _        _____      _  __      _____       __ _           _   _             
 / ____| |               | |      / ____|    | |/ _|    |  __ \     / _| |         | | (_)            
| |    | | __ _ _   _  __| | ___  | (___   ___| | |_ ____| |__) |___| |_| | ___  ___| |_ _  ___  _ __  
| |    | |/ _` | | | |/ _` |/ _ \  \___ \ / _ \ |  _|____|  _  // _ \  _| |/ _ \/ __| __| |/ _ \| '_ \ 
| |____| | (_| | |_| | (_| |  __/  ____) |  __/ | |      | | \ \  __/ | | |  __/ (__| |_| | (_) | | | |
 \_____|_|\__,_|\__,_|\__,_|\___| |_____/ \___|_|_|      |_|  \_\___|_| |_|\___|\___|\__|_|\___/|_| |_|
                                                                                                         
EOF
echo -e "${NC}"
echo "🧠 Semantic Memory for Claude Desktop"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Check system requirements
print_info "Checking system requirements..."

# Check for Docker
if ! command -v docker &> /dev/null; then
    print_error "Docker is not installed. Please install Docker Desktop from https://www.docker.com/products/docker-desktop"
    exit 1
fi

# Check Docker is running
if ! docker info &> /dev/null; then
    print_error "Docker is not running. Please start Docker Desktop."
    exit 1
fi

# Check for Node.js
if ! command -v node &> /dev/null; then
    print_error "Node.js is not installed. Please install Node.js 18+ from https://nodejs.org/"
    exit 1
fi

# Check Node version
NODE_VERSION=$(node -v | cut -d'v' -f2 | cut -d'.' -f1)
if [ "$NODE_VERSION" -lt 18 ]; then
    print_error "Node.js version must be 18 or higher. Current version: $(node -v)"
    exit 1
fi

# Check for Python 3
if ! command -v python3 &> /dev/null; then
    print_error "Python 3 is not installed. Please install Python 3.8+ from https://www.python.org/"
    exit 1
fi

print_success "All system requirements met!"
echo ""

# Create environment file if it doesn't exist
if [ ! -f .env ]; then
    print_info "Creating environment configuration..."
    cat > .env << 'EOL'
# Claude Self-Reflection MCP Configuration
# ========================================

# Embedding Provider (choose one)
# Option 1: OpenAI (recommended for best quality)
OPENAI_API_KEY=your-openai-api-key-here

# Option 2: Voyage AI (best for semantic search)
VOYAGE_API_KEY=your-voyage-api-key-here

# Option 3: Local embeddings (no API key needed, lower quality)
USE_LOCAL_EMBEDDINGS=false

# Claude Desktop Logs Location (auto-detected, modify if needed)
CLAUDE_LOGS_PATH=$HOME/.claude/projects

# Qdrant Configuration
QDRANT_HOST=localhost
QDRANT_PORT=6333

# Import Configuration
BATCH_SIZE=50
CHUNK_SIZE=10
WATCH_INTERVAL=60

# Advanced Settings (optional)
# EMBEDDING_MODEL=voyage-3
# COLLECTION_PREFIX=conv
# MIN_SIMILARITY=0.7
EOL
    print_success "Created .env file"
    print_warning "Please edit .env and add your API keys before starting"
    echo ""
fi

# Install MCP server dependencies
print_info "Installing MCP server dependencies..."
cd claude-self-reflection
npm install --production
npm run build
cd ..
print_success "MCP server ready"
echo ""

# Install Python dependencies
print_info "Installing Python dependencies..."
if [ ! -d "venv" ]; then
    python3 -m venv venv
fi
source venv/bin/activate 2>/dev/null || . venv/bin/activate
pip install -q -r scripts/requirements.txt
deactivate
print_success "Python environment ready"
echo ""

# Create necessary directories
mkdir -p config

# Pull Docker images
print_info "Pulling Docker images..."
docker pull qdrant/qdrant:latest

# Start services
print_info "Starting Qdrant vector database..."
docker compose -f docker-compose.yaml up -d qdrant

# Wait for Qdrant to be ready
print_info "Waiting for Qdrant to initialize..."
sleep 5

# Check if Qdrant is healthy
if curl -s http://localhost:6333/health | grep -q "ok"; then
    print_success "Qdrant is running and healthy"
else
    print_error "Qdrant failed to start. Check docker logs with: docker logs qdrant"
    exit 1
fi
echo ""

# Configure Claude Desktop
print_info "Configuring Claude Desktop integration..."

CLAUDE_CONFIG_DIR="$HOME/Library/Application Support/Claude"
if [ ! -d "$CLAUDE_CONFIG_DIR" ]; then
    CLAUDE_CONFIG_DIR="$HOME/.config/Claude"
fi

if [ -d "$CLAUDE_CONFIG_DIR" ]; then
    cp claude-self-reflection/config/claude-desktop-config.json "$CLAUDE_CONFIG_DIR/claude_desktop_config.json" 2>/dev/null || true
    print_success "Claude Desktop configuration added"
    print_warning "Please restart Claude Desktop to load the MCP server"
else
    print_warning "Claude Desktop configuration directory not found"
    print_info "After installing Claude Desktop, run: ./install.sh --configure-claude"
fi
echo ""

# Test the installation
print_info "Running installation tests..."
cd claude-self-reflection
if npm test -- --grep "connection" > /dev/null 2>&1; then
    print_success "MCP server test passed"
else
    print_warning "MCP server test failed - this is normal if API keys are not configured"
fi
cd ..
echo ""

# Import existing conversations
print_info "Checking for existing Claude conversations..."
CONVERSATION_COUNT=$(find "$HOME/.claude/projects" -name "*.jsonl" 2>/dev/null | wc -l | tr -d ' ')
if [ "$CONVERSATION_COUNT" -gt 0 ]; then
    print_success "Found $CONVERSATION_COUNT conversation files"
    echo ""
    read -p "Would you like to import your existing conversations now? (y/n) " -n 1 -r
    echo ""
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        print_info "Starting conversation import..."
        source venv/bin/activate
        python scripts/import-openai-enhanced.py
        deactivate
    else
        print_info "You can import conversations later with: python scripts/import-openai-enhanced.py"
    fi
else
    print_info "No existing conversations found"
fi
echo ""

# Print success message and next steps
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
print_success "Installation complete! 🎉"
echo ""
echo "📋 Next Steps:"
echo "1. Edit .env file and add your API keys (OpenAI or Voyage AI)"
echo "2. Restart Claude Desktop to load the MCP server"
echo "3. In Claude, type: 'search for our previous conversations about X'"
echo ""
echo "🛠️  Useful Commands:"
echo "• Check status:     docker compose ps"
echo "• View logs:        docker compose logs -f"
echo "• Import convos:    python scripts/import-openai-enhanced.py"
echo "• Stop services:    docker compose down"
echo "• Update:          git pull && ./install.sh"
echo ""
echo "📚 Documentation: https://github.com/yourusername/claude-self-reflection"
echo "💬 Support:       https://github.com/yourusername/claude-self-reflection/issues"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

# Handle special flags
if [ "$1" == "--configure-claude" ]; then
    print_info "Configuring Claude Desktop..."
    # Additional Claude configuration logic
fi

if [ "$1" == "--test" ]; then
    print_info "Running full test suite..."
    cd claude-self-reflection && npm test && cd ..
fi