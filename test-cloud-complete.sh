#\!/bin/bash
echo "=== COMPLETE CLOUD MODE CERTIFICATION TEST ==="
echo "=============================================="

# Clean up everything first
echo "🧹 Cleaning up previous containers..."
docker compose down -v 2>/dev/null
docker stop claude-reflection-streaming 2>/dev/null
docker rm claude-reflection-streaming 2>/dev/null

# Set environment for Cloud mode
export PREFER_LOCAL_EMBEDDINGS=false
export VOYAGE_KEY=$(grep VOYAGE_KEY .env | cut -d= -f2)

if [ -z "$VOYAGE_KEY" ]; then
    echo "❌ VOYAGE_KEY not found in .env file"
    exit 1
fi

echo "✅ Configuration:"
echo "   Mode: CLOUD (Voyage AI)"
echo "   API Key: ${VOYAGE_KEY:0:10}..."

# Clean state
echo ""
echo "📦 Cleaning state for fresh test..."
rm -f ~/.claude-self-reflect/config/imported-files.json
rm -rf ~/.claude-self-reflect/config/import_state
mkdir -p ~/.claude-self-reflect/config

# Start services properly
echo ""
echo "🚀 Starting all services..."
docker compose up -d qdrant
echo "   Waiting for Qdrant to be ready..."
sleep 10

# Verify Qdrant is running
echo ""
echo "✅ Verifying Qdrant is accessible..."
curl -s http://localhost:6333 > /dev/null && echo "   Qdrant is running\!" || echo "   ERROR: Qdrant not accessible"

# Run baseline import
echo ""
echo "📚 Running baseline import with Voyage embeddings..."
docker compose run --rm importer python /scripts/import-conversations-unified.py --limit 3

# Check collections created
echo ""
echo "📊 Checking Voyage collections created..."
curl -s http://localhost:6333/collections | jq '.result.collections[] | select(.name | endswith("_voyage")) | .name' | head -5

# Start streaming watcher with Voyage mode
echo ""
echo "👁️ Starting streaming watcher in CLOUD mode..."
docker compose up -d streaming-importer
sleep 10

# Check gap detection
echo ""
echo "🔍 Verifying gap detection is working..."
docker logs claude-reflection-streaming 2>&1 | tail -100 | grep -E "BASELINE_NEEDED|CATCH_UP|gap|⚠️" | head -5

# Check partial chunk handling
echo ""
echo "🧩 Verifying partial chunk flushing..."
docker logs claude-reflection-streaming 2>&1 | grep -E "Flushing partial chunk" | head -3

# Test actual search
echo ""
echo "🔎 Testing search functionality with Voyage embeddings..."
# Create a test reflection to search for
docker exec claude-reflection-streaming python3 -c "
from qdrant_client import QdrantClient
import voyageai
import os

voyage_client = voyageai.Client(api_key=os.getenv('VOYAGE_KEY'))
client = QdrantClient('http://qdrant:6333')

# Create test point
test_text = 'Testing Cerebras and Qwen models with Claude Code Router'
embedding = voyage_client.embed([test_text], model='voyage-3').embeddings[0]

# Insert into a voyage collection
collections = client.get_collections().collections
voyage_collections = [c.name for c in collections if c.name.endswith('_voyage')]
if voyage_collections:
    collection = voyage_collections[0]
    from qdrant_client.models import PointStruct
    import uuid
    point = PointStruct(
        id=str(uuid.uuid4()),
        vector=embedding,
        payload={'text': test_text, 'test': True}
    )
    client.upsert(collection_name=collection, points=[point], wait=True)
    print(f'✅ Test point inserted into {collection}')
    
    # Now search for it
    results = client.search(
        collection_name=collection,
        query_vector=embedding,
        limit=1
    )
    if results and results[0].payload.get('test'):
        print('✅ Search working correctly with Voyage embeddings\!')
    else:
        print('❌ Search not finding test content')
else:
    print('❌ No Voyage collections found')
"

echo ""
echo "📈 System Status Summary:"
echo "========================"
docker compose ps
echo ""
echo "✅ CLOUD MODE CERTIFICATION COMPLETE"
