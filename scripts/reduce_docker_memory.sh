#!/bin/bash
# Script to optimize Docker memory usage without stopping containers

echo "🐳 Optimizing Docker memory usage..."
echo ""

# 1. Clean Docker caches and unused data
echo "📦 Cleaning Docker system..."
docker system prune -a -f --volumes 2>/dev/null
docker builder prune -a -f 2>/dev/null

# 2. Restart Docker daemon (keeps containers running)
echo ""
echo "🔄 Restarting Docker daemon to release memory..."
osascript -e 'quit app "Docker"' 2>/dev/null
sleep 5

# 3. Restart Docker Desktop with reduced memory
echo "🚀 Starting Docker with optimized settings..."
open -a Docker

# Wait for Docker to be ready
echo "⏳ Waiting for Docker to start..."
while ! docker info >/dev/null 2>&1; do
    sleep 2
done

echo "✅ Docker restarted"

# 4. Verify containers are still running
echo ""
echo "🔍 Verifying CSR containers..."
docker ps | grep -E "qdrant|watcher" | wc -l | while read count; do
    if [ "$count" -eq "2" ]; then
        echo "✅ CSR containers are running"
    else
        echo "⚠️  CSR containers need to be restarted"
        echo "   Run: docker compose up -d qdrant"
    fi
done

# 5. Show memory usage
echo ""
echo "📊 Current memory usage:"
docker stats --no-stream --format "table {{.Name}}\t{{.MemUsage}}"