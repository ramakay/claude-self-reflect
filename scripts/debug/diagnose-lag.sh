#!/bin/bash

# Diagnostic script to trace typing lag in Claude
echo "Starting Claude typing lag diagnosis..."
echo "Will capture for 10 seconds. Start typing in Claude now!"
echo "=========================================="

DIAG_DIR="/tmp/claude-lag-diagnosis"
rm -rf "$DIAG_DIR"
mkdir -p "$DIAG_DIR"

# Capture processes every 500ms
for i in {1..20}; do
    echo "Sample $i/20 at $(date +%H:%M:%S.%3N)"

    # High CPU processes
    ps aux | head -1 > "$DIAG_DIR/sample_${i}.txt"
    ps aux | sort -rn -k 3 | head -20 >> "$DIAG_DIR/sample_${i}.txt"

    # Claude-specific processes
    echo -e "\n--- Claude/Python/Node processes ---" >> "$DIAG_DIR/sample_${i}.txt"
    ps aux | grep -E "(Claude|python|node|mcp|qdrant)" | grep -v grep >> "$DIAG_DIR/sample_${i}.txt"

    # Virtual machine processes
    echo -e "\n--- VM processes ---" >> "$DIAG_DIR/sample_${i}.txt"
    ps aux | grep -i "virtual" | grep -v grep >> "$DIAG_DIR/sample_${i}.txt"

    sleep 0.5
done

echo ""
echo "Analysis complete! Results in $DIAG_DIR"
echo ""
echo "Top CPU consumers across all samples:"
cat "$DIAG_DIR"/sample_*.txt | grep -v "^USER" | awk '{print $3, $11}' | sort -rn | head -20