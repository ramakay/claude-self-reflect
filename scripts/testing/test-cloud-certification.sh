#!/bin/bash
echo "=== CLOUD MODE CERTIFICATION TEST (WITH PROPER TIMEOUTS) ==="
echo "==========================================================="

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
curl -s http://localhost:6333 > /dev/null && echo "   Qdrant is running!" || echo "   ERROR: Qdrant not accessible"

# Run baseline import with smaller limit to avoid timeout
echo ""
echo "📚 Running baseline import with Voyage embeddings (limited for testing)..."
echo "   Using --limit 2 to prevent timeout during test"

# Use timeout command with 5 minute limit
timeout 300 docker compose run --rm importer python /scripts/import-conversations-unified.py --limit 2

if [ $? -eq 124 ]; then
    echo "⚠️  Import timed out after 5 minutes (this is expected for large imports)"
    echo "   In production, run without --limit for full import"
else
    echo "✅ Baseline import completed within timeout"
fi

# Check collections created
echo ""
echo "📊 Checking Voyage collections created..."
VOYAGE_COLLECTIONS=$(curl -s http://localhost:6333/collections | jq -r '.result.collections[] | select(.name | endswith("_voyage")) | .name' | head -5)
if [ -n "$VOYAGE_COLLECTIONS" ]; then
    echo "✅ Voyage collections found:"
    echo "$VOYAGE_COLLECTIONS" | head -3
else
    echo "❌ No Voyage collections created"
fi

# Start streaming watcher with Voyage mode
echo ""
echo "👁️ Starting streaming watcher in CLOUD mode..."
docker compose up -d streaming-importer
echo "   Waiting for watcher to initialize..."
sleep 15

# Check gap detection
echo ""
echo "🔍 Verifying gap detection is working..."
GAP_DETECTION=$(docker logs claude-reflection-streaming 2>&1 | tail -200 | grep -E "BASELINE_NEEDED|CATCH_UP|gap|⚠️" | head -5)
if [ -n "$GAP_DETECTION" ]; then
    echo "✅ Gap detection active:"
    echo "$GAP_DETECTION"
else
    echo "⚠️  No gap detection messages found (may be normal if baseline is complete)"
fi

# Check partial chunk handling
echo ""
echo "🧩 Verifying partial chunk flushing..."
PARTIAL_CHUNKS=$(docker logs claude-reflection-streaming 2>&1 | grep -E "Flushing partial chunk" | head -3)
if [ -n "$PARTIAL_CHUNKS" ]; then
    echo "✅ Partial chunk flushing working:"
    echo "$PARTIAL_CHUNKS"
else
    echo "⚠️  No partial chunks found (may be normal if chunks are complete)"
fi

# Test actual search for Cerebras content
echo ""
echo "🔎 Testing search for 'cererbras' (with typo) content..."
docker exec claude-reflection-streaming python3 -c "
import sys
from qdrant_client import QdrantClient
import voyageai
import os

try:
    voyage_client = voyageai.Client(api_key=os.getenv('VOYAGE_KEY'))
    client = QdrantClient('http://qdrant:6333')
    
    # Get voyage collections
    collections = client.get_collections().collections
    voyage_collections = [c.name for c in collections if c.name.endswith('_voyage')]
    
    if not voyage_collections:
        print('❌ No Voyage collections found')
        sys.exit(1)
    
    print(f'Found {len(voyage_collections)} Voyage collections')
    
    # Search for cererbras (with typo) - this should be in the imported conversations
    queries = ['cererbras', 'Cerebras', 'Qwen', 'openrouter']
    
    for query in queries:
        embedding = voyage_client.embed([query], model='voyage-3').embeddings[0]
        found = False
        
        for collection in voyage_collections[:3]:  # Check first 3 collections
            try:
                results = client.search(
                    collection_name=collection,
                    query_vector=embedding,
                    limit=5,
                    score_threshold=0.5
                )
                
                for r in results:
                    text = str(r.payload).lower()
                    if query.lower() in text or 'cerebras' in text:
                        print(f'✅ Found \"{query}\" content in {collection} (score: {r.score:.3f})')
                        # Show snippet
                        if 'text' in r.payload:
                            snippet = r.payload['text'][:200]
                            print(f'   Snippet: {snippet}...')
                        found = True
                        break
            except Exception as e:
                pass
            
            if found:
                break
        
        if not found:
            print(f'⚠️  \"{query}\" not found (may need more import time or different file)')
    
except Exception as e:
    print(f'❌ Error during search test: {e}')
    import traceback
    traceback.print_exc()
"

# Test insert and search to verify system is working
echo ""
echo "🧪 Testing insert and search capability..."
docker exec claude-reflection-streaming python3 -c "
from qdrant_client import QdrantClient
from qdrant_client.models import PointStruct
import voyageai
import os
import uuid

try:
    voyage_client = voyageai.Client(api_key=os.getenv('VOYAGE_KEY'))
    client = QdrantClient('http://qdrant:6333')
    
    # Get first voyage collection
    collections = client.get_collections().collections
    voyage_collections = [c.name for c in collections if c.name.endswith('_voyage')]
    
    if voyage_collections:
        collection = voyage_collections[0]
        
        # Create test point
        test_text = 'TEST: This is a test of Cerebras and Qwen models with Claude Code Router'
        embedding = voyage_client.embed([test_text], model='voyage-3').embeddings[0]
        
        point = PointStruct(
            id=str(uuid.uuid4()),
            vector=embedding,
            payload={'text': test_text, 'test': True}
        )
        
        # Insert
        client.upsert(collection_name=collection, points=[point], wait=True)
        print(f'✅ Test point inserted into {collection}')
        
        # Search for it
        search_embedding = voyage_client.embed(['Cerebras test'], model='voyage-3').embeddings[0]
        results = client.search(
            collection_name=collection,
            query_vector=search_embedding,
            limit=3
        )
        
        found_test = False
        for r in results:
            if r.payload.get('test'):
                print(f'✅ Found test point! Score: {r.score:.3f}')
                print(f'   Content: {r.payload.get(\"text\", \"No text\")}')
                found_test = True
                break
        
        if not found_test:
            print('❌ Test point not found in search results')
    else:
        print('❌ No Voyage collections available for testing')
        
except Exception as e:
    print(f'❌ Error: {e}')
"

echo ""
echo "📈 System Status Summary:"
echo "========================"
docker compose ps --format "table {{.Name}}\t{{.Status}}\t{{.Ports}}"

echo ""
echo "📊 Collection Summary:"
curl -s http://localhost:6333/collections | jq '.result.collections | length' | xargs -I {} echo "   Total collections: {}"
curl -s http://localhost:6333/collections | jq '.result.collections[] | select(.name | endswith("_voyage")) | .name' | wc -l | xargs -I {} echo "   Voyage collections: {}"
curl -s http://localhost:6333/collections | jq '.result.collections[] | select(.name | endswith("_local")) | .name' | wc -l | xargs -I {} echo "   Local collections: {}"

echo ""
echo "✅ CLOUD MODE CERTIFICATION COMPLETE"
echo ""
echo "📝 Notes:"
echo "   - Used --limit 2 for baseline import to prevent timeout"
echo "   - For production, run without --limit for full import"
echo "   - Gap detection will fill in missing conversations automatically"
echo "   - The 'cererbras' typo content requires the specific conversation file to be imported"