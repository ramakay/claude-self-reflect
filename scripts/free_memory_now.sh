#!/bin/bash
# Emergency memory freeing script

echo "🚨 MEMORY OPTIMIZATION SCRIPT"
echo "=============================="
echo ""

# 1. Show current state
echo "📊 Current Memory State:"
sysctl vm.swapusage | grep -o "used = [^M]*M"
echo ""

# 2. Kill old Chrome processes
echo "🌐 Cleaning old Chrome processes..."
OLD_CHROME=$(ps aux | grep "Google Chrome" | grep -E "Mon|Tue|Wed|Thu|Fri|Sat|Sun" | awk '{print $2}' | wc -l)
echo "   Found $OLD_CHROME old Chrome processes"

# Kill Chrome render processes older than today
ps aux | grep "Google Chrome" | grep -E "Mon|Tue|Wed|Thu|Fri|Sat|Sun" | awk '{print $2}' | while read pid; do
    kill -9 $pid 2>/dev/null
done

# 3. Clear system caches
echo ""
echo "🧹 Clearing system caches (requires password)..."
sudo purge

# 4. Force memory compaction
echo ""
echo "💾 Compacting memory..."
sudo memory_pressure

# 5. Show results
echo ""
echo "✅ RESULTS:"
echo "==========="
sysctl vm.swapusage
echo ""
echo "Top memory users now:"
ps aux | sort -nrk 6 | head -5 | awk '{printf "%-20s %8s\n", substr($11,0,20), $6/1024"MB"}'

echo ""
echo "🎯 Next steps:"
echo "1. Test typing in Claude Code now"
echo "2. If still slow, close more Chrome tabs"
echo "3. Consider restarting Chrome completely"