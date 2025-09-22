#!/bin/bash

# Deep trace for Claude typing lag - focus on statusline and subprocess calls
echo "=== DEEP CLAUDE LAG TRACE ==="
echo "Monitoring for 15 seconds. Start typing NOW!"
echo "Watch for freezes when statusline shifts..."
echo ""

# Kill any existing traces
pkill -f "fs_usage.*Claude" 2>/dev/null

# Output directory
TRACE_DIR="/tmp/claude-deep-trace-$(date +%s)"
mkdir -p "$TRACE_DIR"

echo "Trace output: $TRACE_DIR"
echo "----------------------------------------"

# 1. Monitor file system calls (most likely culprit)
echo "[1/5] Starting file system trace..."
sudo fs_usage -w -f filesys | grep -E "(Claude|csr-status|python|node|bash)" > "$TRACE_DIR/fs_trace.log" 2>&1 &
FS_PID=$!

# 2. Monitor process spawning (statusline updates spawn subprocesses)
echo "[2/5] Starting process spawn trace..."
sudo dtrace -qn 'proc:::exec-success { printf("%Y %s\n", walltimestamp, execname); }' > "$TRACE_DIR/exec_trace.log" 2>&1 &
DTRACE_PID=$!

# 3. Sample Claude's activity every 100ms
echo "[3/5] Starting rapid sampling (100ms intervals)..."
(
    for i in {1..150}; do
        echo "=== Sample $i at $(date +%H:%M:%S.%3N) ===" >> "$TRACE_DIR/rapid_sample.log"

        # Check what Claude is doing RIGHT NOW
        sample Claude 2>&1 | head -50 >> "$TRACE_DIR/rapid_sample.log"

        # Check statusline processes
        ps aux | grep -E "(csr-status|statusline|ccstatusline)" | grep -v grep >> "$TRACE_DIR/rapid_sample.log"

        # Check if any blocking operations
        lsof -p $(pgrep Claude | head -1) 2>/dev/null | grep -E "(PIPE|FIFO|REG.*\.json)" >> "$TRACE_DIR/rapid_sample.log"

        sleep 0.1
    done
) &
SAMPLE_PID=$!

# 4. Monitor statusline script calls specifically
echo "[4/5] Monitoring statusline updates..."
(
    sudo fs_usage -w | grep -E "(csr-status|opus-time|realtime_quality\.json|statusline)" > "$TRACE_DIR/statusline_trace.log" 2>&1
) &
STATUS_PID=$!

# 5. Check for Python/Node subprocess spawning
echo "[5/5] Monitoring subprocess creation..."
(
    while true; do
        ps aux | grep -E "(python.*claude|node.*claude)" | grep -v grep >> "$TRACE_DIR/subprocess_spawn.log"
        echo "---$(date +%H:%M:%S.%3N)---" >> "$TRACE_DIR/subprocess_spawn.log"
        sleep 0.2
    done
) &
SUB_PID=$!

# Let it run for 15 seconds
echo ""
echo "Recording... Type in Claude now to reproduce the lag!"
echo "(Press Ctrl+C to stop early if you captured the lag)"
echo ""

# Progress bar
for i in {1..15}; do
    echo -n "."
    sleep 1
done
echo ""

# Stop all traces
echo "Stopping traces..."
sudo kill $FS_PID $DTRACE_PID $STATUS_PID 2>/dev/null
kill $SAMPLE_PID $SUB_PID 2>/dev/null

# Analyze results
echo ""
echo "=== ANALYSIS ==="

echo "1. Subprocess spawns during trace:"
grep -c "exec-success" "$TRACE_DIR/exec_trace.log" 2>/dev/null || echo "0"

echo ""
echo "2. Statusline file operations:"
grep -c "csr-status" "$TRACE_DIR/fs_trace.log" 2>/dev/null || echo "0"

echo ""
echo "3. Most accessed files:"
cat "$TRACE_DIR/fs_trace.log" 2>/dev/null | awk '{print $NF}' | sort | uniq -c | sort -rn | head -5

echo ""
echo "4. Process spawn frequency:"
cat "$TRACE_DIR/exec_trace.log" 2>/dev/null | awk '{print $2}' | sort | uniq -c | sort -rn | head -5

echo ""
echo "5. Blocking operations detected:"
grep -E "(PIPE|FIFO)" "$TRACE_DIR/rapid_sample.log" 2>/dev/null | wc -l

echo ""
echo "=== KEY FINDINGS ==="
echo "Check these files for detailed analysis:"
echo "  $TRACE_DIR/fs_trace.log         - File system calls"
echo "  $TRACE_DIR/exec_trace.log       - Process spawning"
echo "  $TRACE_DIR/statusline_trace.log - Statusline activity"
echo "  $TRACE_DIR/rapid_sample.log     - Claude samples"
echo "  $TRACE_DIR/subprocess_spawn.log - Subprocess creation"

echo ""
echo "Look for:"
echo "  - Burst of file operations when typing freezes"
echo "  - Subprocess spawning during lag"
echo "  - Statusline refresh patterns"