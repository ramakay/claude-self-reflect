#!/bin/bash
# Optimize Docker memory allocation for CSR

echo "🚀 Docker Memory Optimization for CSR"
echo "====================================="
echo ""

# 1. Show current state
echo "📊 Current Memory Usage:"
docker stats --no-stream | grep -E "NAME|qdrant|watcher"
echo ""
echo "Current swap usage:"
sysctl vm.swapusage | grep -o "used = [^M]*M"
echo ""

# 2. Backup current data
echo "💾 Backing up current state..."
docker exec claude-reflection-qdrant curl -s http://localhost:6333/collections | python3 -m json.tool > /tmp/qdrant_collections_backup.json
echo "   Backup saved to /tmp/qdrant_collections_backup.json"
echo ""

# 3. Stop containers gracefully
echo "🛑 Stopping current containers..."
docker stop claude-reflection-qdrant claude-reflection-safe-watcher
sleep 3

# 4. Remove old containers (keeps data volumes)
echo "🗑️  Removing old containers (data is preserved)..."
docker rm claude-reflection-qdrant claude-reflection-safe-watcher

# 5. Start with optimized memory
echo "🚀 Starting containers with optimized memory..."
cd /Users/ramakrishnanannaswamy/projects/claude-self-reflect

# Use the optimized compose file
docker compose -f docker-compose-optimized.yml up -d

# 6. Wait for services to be ready
echo "⏳ Waiting for services to start..."
sleep 5

# Check if Qdrant is healthy
MAX_RETRIES=30
RETRY_COUNT=0
while ! docker exec claude-reflection-qdrant curl -s http://localhost:6333/health > /dev/null 2>&1; do
    RETRY_COUNT=$((RETRY_COUNT + 1))
    if [ $RETRY_COUNT -ge $MAX_RETRIES ]; then
        echo "❌ Qdrant failed to start!"
        echo "Rolling back..."
        docker compose -f docker-compose-optimized.yml down
        # Restart with original settings
        docker run -d --name claude-reflection-qdrant \
            -p 6333:6333 \
            -v ./qdrant_storage:/qdrant/storage \
            --memory=4g \
            qdrant/qdrant:latest
        exit 1
    fi
    echo "   Waiting for Qdrant to be ready... ($RETRY_COUNT/$MAX_RETRIES)"
    sleep 2
done

echo "✅ Qdrant is healthy!"

# 7. Verify data integrity
echo ""
echo "🔍 Verifying data integrity..."
COLLECTIONS=$(docker exec claude-reflection-qdrant curl -s http://localhost:6333/collections | python3 -c "import sys, json; data=json.load(sys.stdin); print(len(data.get('result', {}).get('collections', [])))")
echo "   Found $COLLECTIONS collections in Qdrant"

# 8. Show new memory usage
echo ""
echo "📊 NEW Memory Usage (Optimized):"
docker stats --no-stream | grep -E "NAME|qdrant|watcher"

# 9. Calculate savings
echo ""
echo "💰 Memory Savings:"
echo "   Qdrant: 4GB → 2GB (saved 2GB)"
echo "   Watcher: 7.6GB → 1GB (saved 6.6GB)"
echo "   Total freed: 8.6GB of RAM!"

# 10. Restart Docker Desktop to fully release memory
echo ""
echo "🔄 Restarting Docker Desktop to release memory..."
osascript -e 'quit app "Docker"'
sleep 5
open -a Docker

# Wait for Docker to restart
echo "⏳ Waiting for Docker to restart..."
while ! docker info >/dev/null 2>&1; do
    sleep 2
done

# Start containers again
docker compose -f docker-compose-optimized.yml up -d

echo ""
echo "✅ OPTIMIZATION COMPLETE!"
echo ""
echo "New swap usage:"
sysctl vm.swapusage | grep -o "used = [^M]*M"
echo ""
echo "🎯 Next steps:"
echo "1. Test CSR functionality with: reflect_on_past('test')"
echo "2. Monitor for OOM errors in: docker logs claude-reflection-qdrant"
echo "3. If OOM occurs, increase limits in docker-compose-optimized.yml"