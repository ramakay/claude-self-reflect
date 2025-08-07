#\!/bin/bash
echo "=== CLOUD MODE (Voyage AI) CERTIFICATION TEST ==="
echo "================================================"

# Set to use Voyage embeddings
export PREFER_LOCAL_EMBEDDINGS=false
export VOYAGE_KEY=$(grep VOYAGE_KEY .env | cut -d= -f2)

if [ -z "$VOYAGE_KEY" ]; then
    echo "❌ VOYAGE_KEY not found in .env file"
    exit 1
fi

echo "✅ Using Voyage AI embeddings"
echo "   API Key: ${VOYAGE_KEY:0:10}..."

# Clean state for fresh test
echo ""
echo "📦 Cleaning state for fresh test..."
rm -f ~/.claude-self-reflect/config/imported-files.json
rm -rf ~/.claude-self-reflect/config/import_state
mkdir -p ~/.claude-self-reflect/config

# Start Qdrant
echo ""
echo "🚀 Starting Qdrant..."
docker compose up -d qdrant
sleep 5

# Run baseline import
echo ""
echo "📚 Running baseline import with Voyage embeddings..."
docker compose run --rm importer python /scripts/import-conversations-unified.py --limit 5

# Check import results
echo ""
echo "📊 Checking import results..."
docker compose run --rm importer python -c "
from qdrant_client import QdrantClient
client = QdrantClient('http://qdrant:6333')
collections = client.get_collections().collections
voyage_collections = [c.name for c in collections if c.name.endswith('_voyage')]
print(f'Voyage collections created: {len(voyage_collections)}')
for c in voyage_collections[:3]:
    info = client.get_collection(c)
    print(f'  {c}: {info.points_count} points')
"

# Start streaming watcher
echo ""
echo "👁️ Starting streaming watcher..."
docker compose up -d streaming-importer
sleep 10

# Check gap detection
echo ""
echo "🔍 Checking gap detection in watcher logs..."
docker logs claude-reflection-streaming --tail 50 | grep -E "BASELINE_NEEDED|CATCH_UP|gap|⚠️" | head -10

# Test MCP search
echo ""
echo "🔎 Testing MCP search for Cerebras/Qwen content..."
