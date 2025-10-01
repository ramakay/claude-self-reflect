#!/bin/bash
# Safe Docker cleanup script for Claude Self-Reflect project
# This preserves running containers and production volumes

echo "🧹 Docker Cleanup for Claude Self-Reflect"
echo "========================================="

# 1. Stop test Qdrant if no longer needed
read -p "Stop test Qdrant container (port 6334)? [y/N]: " stop_test
if [[ $stop_test =~ ^[Yy]$ ]]; then
    echo "Stopping claude-test-qdrant..."
    docker stop claude-test-qdrant
    docker rm claude-test-qdrant
fi

# 2. Clean up dangling volumes (47GB+ potential savings)
echo ""
echo "📦 Cleaning dangling volumes..."
docker volume ls -f dangling=true
read -p "Remove these dangling volumes? [y/N]: " remove_volumes
if [[ $remove_volumes =~ ^[Yy]$ ]]; then
    docker volume prune -f
    echo "✅ Dangling volumes removed"
fi

# 3. Clean build cache (3.4GB)
echo ""
echo "🔨 Cleaning build cache..."
docker builder prune -f
echo "✅ Build cache cleaned"

# 4. Clean unused images
echo ""
echo "🖼️ Checking for unused images..."
docker images -f "dangling=true"
read -p "Remove unused images? [y/N]: " remove_images
if [[ $remove_images =~ ^[Yy]$ ]]; then
    docker image prune -a -f
    echo "✅ Unused images removed"
fi

# 5. Show final disk usage
echo ""
echo "📊 Final Docker disk usage:"
docker system df

echo ""
echo "✅ Cleanup complete!"
echo ""
echo "⚠️ IMPORTANT: Keep these running:"
echo "  - claude-reflection-qdrant (production Qdrant on port 6333)"
echo "  - Any Supabase containers (different project)"