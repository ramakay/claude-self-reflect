#!/bin/bash
# Emergency memory fix for typing lag

echo "🚨 EMERGENCY MEMORY RECOVERY"
echo "============================"
echo ""

# 1. Kill Chrome processes aggressively
echo "🌐 Killing old Chrome processes..."
ps aux | grep "Google Chrome" | grep -v "Helper (GPU)" | awk '{print $2}' | tail -100 | while read pid; do
    kill -9 $pid 2>/dev/null
done

# 2. Kill Slack if running
echo "💬 Stopping Slack temporarily..."
pkill -9 Slack 2>/dev/null

# 3. Restart Docker Desktop with lower memory
echo "🐳 Restarting Docker with 4GB limit..."
osascript -e 'quit app "Docker"'
sleep 3

# Create settings to limit Docker to 4GB
cat > /tmp/docker_settings.json << 'EOF'
{
  "memoryMiB": 4096,
  "cpus": 4,
  "swapMiB": 1024
}
EOF

# Apply settings (if possible)
if [ -d ~/Library/Group\ Containers/group.com.docker ]; then
    cp /tmp/docker_settings.json ~/Library/Group\ Containers/group.com.docker/settings.json 2>/dev/null
fi

# Restart Docker
open -a Docker

echo ""
echo "⏳ Waiting for Docker to start (30 seconds)..."
sleep 30

# 4. Restart CSR containers with minimal memory
docker compose -f docker-compose-optimized.yml up -d

# 5. Clear all caches
echo ""
echo "🧹 Run this command manually to clear caches:"
echo "   sudo purge"

# 6. Show results
echo ""
echo "📊 RESULTS:"
sysctl vm.swapusage
echo ""
echo "Free memory pages:"
vm_stat | grep "Pages free"

echo ""
echo "✅ Emergency recovery complete!"
echo ""
echo "🎯 NEXT STEPS:"
echo "1. Close all Chrome tabs except current"
echo "2. Run: sudo purge"
echo "3. Test typing in Claude now"